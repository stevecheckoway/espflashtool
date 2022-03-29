use std::borrow::Cow;
use std::cmp::{max, min};
use std::io::{self, BufRead, BufReader, Cursor};
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

use binrw::{BinRead, BinWrite};
use serialport::SerialPort;

use crate::chip::Chip;
use crate::command::{Command, CommandError};
use crate::event::{Event, EventObserver};
use crate::stub::Stub;
use crate::timeout::ErrorExt;
use crate::Result;
use crate::{from_be16, from_le, from_le16, from_le32, Error};

const DEFAULT_SERIAL_TIMEOUT: Duration = Duration::from_millis(10);

const CHIP_MAGIC_REG: u32 = 0x40001000;

#[derive(Clone, Copy, Debug, thiserror::Error)]
pub enum FlasherError {
    #[error("Unknown ESP device ({:08X})", .0)]
    UnknownDevice(u32),

    #[error("Command cannot be sent without setting or detecting chip first")]
    MustSetChipFirst,

    #[error("SPI commands cannot be sent without attaching first")]
    MustSpiAttachFirst,

    #[error("Invalid SPI command or address length")]
    InvalidSpiCommand,

    #[error("Invalid stub hello")]
    InvalidStubHello,
}

pub struct Flasher {
    serial: BufReader<Box<dyn SerialPort>>,
    status_size: usize,
    rom_loader: bool,
    observers: Vec<Weak<dyn EventObserver>>,
    owned_observers: Vec<Rc<dyn EventObserver>>,
    chip: Option<Chip>,
    attached: bool,
}

impl Flasher {
    pub fn new(path: &str) -> Result<Self> {
        let serial = serialport::new(path, 115200).open()?;
        Ok(Flasher {
            serial: BufReader::new(serial),
            status_size: 0,
            rom_loader: true,
            observers: Vec::new(),
            owned_observers: Vec::new(),
            chip: None,
            attached: false,
        })
    }

    pub fn add_observer<E>(&mut self, observer: Weak<E>)
    where
        E: EventObserver + 'static,
    {
        self.observers.push(observer);
    }

    pub fn add_owned_observer<O>(&mut self, observer: O)
    where
        O: Into<Rc<dyn EventObserver + 'static>>,
    {
        let observer = observer.into();
        self.observers.push(Rc::downgrade(&observer));
        self.owned_observers.push(observer);
    }

    pub fn chip(&self) -> Result<Chip> {
        self.chip
            .ok_or_else(|| FlasherError::MustSetChipFirst.into())
    }

    pub fn set_chip(&mut self, chip: Chip) {
        self.chip = Some(chip);
        if !self.rom_loader || chip == Chip::Esp8266 {
            self.status_size = 2;
        } else {
            self.status_size = 4;
        }
    }

    fn serial(&mut self) -> &mut dyn SerialPort {
        self.serial.get_mut().as_mut()
    }

    fn send_packet(&mut self, data: &[u8]) -> Result<()> {
        let mut encoder = slip_codec::SlipEncoder::new(true);
        let mut output: Vec<u8> = Vec::with_capacity(data.len() + 8);

        encoder.encode(data, &mut output)?;
        self.trace(Event::SlipWrite(Cow::from(data)));

        let serial = self.serial();
        serial.set_timeout(DEFAULT_SERIAL_TIMEOUT)?;
        serial.write_all(&output)?;
        self.trace(Event::SerialWrite(Cow::from(&output)));
        Ok(())
    }

    fn read_packet(&mut self, timeout: Duration) -> Result<Vec<u8>> {
        let read_start = Instant::now();
        let mut decoder = slip_codec::SlipDecoder::new();
        let mut response: Vec<u8> = Vec::new();

        self.serial().set_timeout(timeout)?;
        loop {
            let buffer = self.serial.fill_buf()?;
            let mut cursor = Cursor::new(buffer);

            match decoder.decode(&mut cursor, &mut response) {
                Ok(_) => {
                    // A complete packet has been decoded.
                    let size = cursor.position() as usize;
                    self.serial.consume(size);
                    self.trace(Event::SlipRead(Cow::from(&response)));
                    return Ok(response);
                }
                Err(slip_codec::SlipError::EndOfStream) => {
                    // The decoder did not hit the end of the packet but did consume all of buffer.
                    let len = buffer.len();
                    self.serial.consume(len);
                    let remaining_time = timeout.saturating_sub(read_start.elapsed());
                    self.serial()
                        .set_timeout(max(DEFAULT_SERIAL_TIMEOUT, remaining_time))?;
                }
                Err(err) => {
                    panic!("Programming error: decoder.decode() returned {:?}", err);
                }
            }
        }
    }

    #[inline]
    fn send_command(&mut self, cmd: Command) -> Result<(u32, Vec<u8>)> {
        self.send_command_with_data(cmd, &[])
    }

    fn send_command_with_data(&mut self, cmd: Command, data: &[u8]) -> Result<(u32, Vec<u8>)> {
        let mut slip_data: Vec<u8> = Vec::with_capacity(64);
        let cmd_code = cmd.code();
        let mut checksum = 0xEFu8;
        for x in data {
            checksum ^= *x;
        }

        /*
         * A Command packet is sent as a SLIP frame with a header and data.
         * Header
         *   0: Direction, always 0x00
         *   1: Command identifier
         * 2-3: Length of data in little endian
         * 4-7: Checksum for the *Data commands
         *
         * Followed by data.
         */
        slip_data.extend(&[0, cmd_code, 0, 0, checksum, 0, 0, 0]);
        {
            let mut cursor = Cursor::new(&mut slip_data);
            cursor.set_position(8);
            cmd.write_with_args(&mut cursor, (self.rom_loader,))?;
        }
        slip_data.extend(data);

        let len: u16 = (slip_data.len() - 8).try_into().expect("Data too long");
        slip_data[2] = len as u8;
        slip_data[3] = (len >> 8) as u8;
        slip_data[4] = checksum;

        let timeout = cmd.timeout();
        self.trace(Event::Command(cmd, Cow::Borrowed(data)));
        self.send_packet(&slip_data)?;
        let response = self.read_response(cmd_code, timeout);
        if response.is_timeout() {
            self.trace(Event::CommandTimeout(cmd_code));
        }
        response
    }

    // Read a response packet corresponding to the command with code `cmd_code`.
    // The first byte of the response must appear within `timeout` and subsequent
    // bytes must arrive within `DEFAULT_SERIAL_TIMEOUT`.
    fn read_response(&mut self, cmd_code: u8, timeout: Duration) -> Result<(u32, Vec<u8>)> {
        let start_time = Instant::now();
        let expected_response_size =
            Command::response_data_len_from_code(cmd_code, self.rom_loader);

        loop {
            let response = self.read_packet(timeout.saturating_sub(start_time.elapsed()))?;

            if response.len() < 10 // Smallest response packet is 10 or 12 bytes.
                || response[0] != 1 // 1 = response, 0 = command.
                || { // Size is invalid.
                    let size = from_le16(&response[2..4]) as usize;
                    size + 8 != response.len()
                    || match (expected_response_size, self.status_size) {
                        (usize::MAX, 0) => false,
                        (usize::MAX, status_size) => size < status_size,
                        (expected, 0) => size != expected + 2 && size != expected + 4,
                        (expected, status_size) => size != expected + status_size,
                    }
                }
            {
                self.trace(Event::InvalidResponse(Cow::from(&response)));
                continue;
            }

            // The response has an appropriate header and enough data for a
            // status (assuming we know how long the status is).
            let cmd = response[1];
            if self.status_size == 0 {
                // Try to figure out the status size.
                if expected_response_size == usize::MAX {
                    return Err(FlasherError::MustSetChipFirst.into());
                }
                let size = from_le16(&response[2..4]) as usize;
                let status_size = size.saturating_sub(expected_response_size);
                // This was checked above in the match.
                assert!(status_size == 2 || status_size == 4);
                self.status_size = status_size;
            }
            let status = response[response.len() - self.status_size];
            let err = response[response.len() - self.status_size + 1];
            let value = from_le32(&response[4..8]);
            let data = &response[8..response.len() - self.status_size];
            self.trace(Event::Response(cmd, status, err, value, Cow::from(data)));

            if cmd == cmd_code {
                match status {
                    0 => return Ok((value, data.to_vec())),
                    1 => return Err(CommandError::from(err).into()),
                    _ => return Err(CommandError::InvalidResponse.into()),
                }
            }
        }
    }

    // Read a line of text.
    // The first byte of the response must appear within `timeout` and subsequent
    // bytes must arrive within `DEFAULT_SERIAL_TIMEOUT`.
    pub fn read_line(&mut self, timeout: Duration) -> Result<Vec<u8>> {
        let read_start = Instant::now();
        let mut line: Vec<u8> = Vec::new();

        self.serial().set_timeout(timeout)?;
        loop {
            let buffer = self.serial.fill_buf()?;

            if let Some(idx) = buffer.iter().position(|&x| x == b'\n') {
                line.extend(&buffer[..idx + 1]);
                self.serial.consume(idx + 1);
                self.trace(Event::SerialLine(Cow::from(&line)));
                return Ok(line);
            }
            line.extend(buffer);
            let len = buffer.len();
            self.serial.consume(len);
            let remaining_time = timeout.saturating_sub(read_start.elapsed());
            self.serial()
                .set_timeout(max(DEFAULT_SERIAL_TIMEOUT, remaining_time))?;
        }
    }

    fn trace(&mut self, event: Event) {
        let now = Instant::now();
        // Remove any observers that have been dropped and notify the others.
        self.observers.retain(|observer| {
            Weak::upgrade(observer).map_or(false, |observer| {
                observer.notify(now, &event);
                true
            })
        });
    }

    fn trace_only(&self, event: Event) {
        let now = Instant::now();
        for observer in &self.observers {
            if let Some(observer) = Weak::upgrade(observer) {
                observer.notify(now, &event);
            }
        }
    }

    pub fn connect(&mut self) -> Result<()> {
        let timeout = Duration::from_millis(100);
        let mut waiting = false;
        'outer: for _ in 0..10 {
            self.reset(true)?;
            // Look for boot message.
            for _ in 0..10 {
                let line = self.read_line(timeout);
                if line.is_timeout() || line? == b"waiting for download\r\n" {
                    // XXX: Does the ESP8266 write this message but just at a different baud rate?
                    waiting = true;
                    break 'outer;
                }
            }
        }
        if !waiting {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Timed out waiting for \"waiting for download\\r\\n\"",
            )
            .into());
        }
        for _ in 0..10 {
            let result = self.sync();
            if result.is_timeout() {
                continue;
            }
            result?;
            break;
        }

        Ok(())
    }

    pub fn attach(&mut self) -> Result<()> {
        if self.chip()? == Chip::Esp8266 {
            self.flash_begin(0, 0, 0, 0)
        } else {
            self.spi_attach()
        }
    }

    pub fn flash_id(&mut self) -> Result<(u8, u16)> {
        let mut output = [0u8; 3];
        self.spi_command(0x9F, 1, 0, 0, 0, &[], &mut output)?;
        Ok((output[0], from_be16(&output[1..3])))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn spi_command(
        &mut self,
        command: u16,
        command_len: u32,
        address: u32,
        address_len: u32,
        dummy_cycles: u32,
        data: &[u8],
        output: &mut [u8],
    ) -> Result<()> {
        if !matches!(command_len, 1 | 2)
            || !matches!(address_len, 0..=4)
            || !matches!(dummy_cycles, 0..=255)
            || data.len() > 64
            || output.len() > 64
        {
            return Err(FlasherError::InvalidSpiCommand.into());
        }
        let chip = self.chip()?;
        if !self.attached {
            self.spi_attach()?;
        }

        // SPI_CMD_REG
        const SPI_USR: u32 = 1 << 18;

        // SPI_USER_REG
        const SPI_USR_COMMAND: u32 = 1 << 31;
        const SPI_USR_ADDR: u32 = 1 << 30;
        const SPI_USR_DUMMY: u32 = 1 << 29;
        const SPI_USR_MISO: u32 = 1 << 28;
        const SPI_USR_MOSI: u32 = 1 << 27;
        let regs = chip.spi_regs();

        let mut user_data = SPI_USR_COMMAND;
        let mut user1_data = 0;
        let command = if command_len == 1 {
            command
        } else {
            command.to_be()
        } as u32;
        let user2_data = (command_len * 8 - 1) << 28 | command;
        self.write_reg(regs.user2, user2_data, 0xFFFFFFFF, 0)?;

        if address_len > 0 {
            user_data |= SPI_USR_ADDR;
            user1_data |= (address_len * 8 - 1) << 26;
            let address = match address_len {
                1 => address,
                2 => (address as u16).to_be() as u32,
                3 => {
                    ((address & 0xFF0000) >> 16)
                        | (address & 0x00FF00)
                        | ((address & 0x0000FF) << 16)
                }
                4 => address.to_be(),
                _ => unreachable!(),
            };
            self.write_reg(regs.addr, address, 0xFFFFFFFF, 0)?;
        }
        if dummy_cycles > 0 {
            user_data |= SPI_USR_DUMMY;
            user1_data |= dummy_cycles - 1;
        }
        if !data.is_empty() {
            user_data |= SPI_USR_MOSI;
            let data_len = (data.len() * 8 - 1) as u32;
            if chip == Chip::Esp8266 {
                user1_data |= data_len << 17;
            } else {
                self.write_reg(regs.mosi_dlen, data_len, 0xFFFFFFFF, 0)?;
            }

            for (pos, val) in data.chunks(4).enumerate() {
                let val = from_le(val);
                self.write_reg(regs.w(pos), val, 0xFFFFFFFF, 0)?;
            }
        }
        if !output.is_empty() {
            user_data |= SPI_USR_MISO;
            let output_len = (output.len() * 8 - 1) as u32;
            if chip == Chip::Esp8266 {
                user1_data |= output_len << 8;
            } else {
                self.write_reg(regs.miso_dlen, output_len, 0xFFFFFFFF, 0)?;
            }
        }
        self.write_reg(regs.user1, user1_data, 0xFFFFFFFF, 0)?;
        self.write_reg(regs.user, user_data, 0xFFFFFFFF, 0)?;
        self.write_reg(regs.cmd, SPI_USR, 0xFFFFFFFF, 0)?;

        loop {
            let cmd = self.read_reg(regs.cmd)?;
            if cmd & SPI_USR == 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }

        // Read output.
        for (pos, output_val) in output.chunks_mut(4).enumerate() {
            let val = self.read_reg(regs.w(pos))?.to_le_bytes();
            output_val.copy_from_slice(&val[..output_val.len()]);
        }
        Ok(())
    }

    pub fn reset(&mut self, enter_bootloader: bool) -> Result<()> {
        self.trace(Event::Reset);
        self.serial.consume(self.serial.buffer().len());

        let serial = self.serial();
        serial.clear(serialport::ClearBuffer::All)?;

        // /RTS is connected to EN
        // /DTR is connected to GPIO0
        serial.write_request_to_send(true)?;
        serial.write_data_terminal_ready(false)?;
        std::thread::sleep(Duration::from_millis(100));
        serial.clear(serialport::ClearBuffer::All)?;

        serial.write_data_terminal_ready(enter_bootloader)?;
        serial.write_request_to_send(false)?;
        std::thread::sleep(Duration::from_millis(500));
        serial.write_data_terminal_ready(false)?;

        Ok(())
    }

    pub fn flash_begin(
        &mut self,
        total_size: u32,
        num_packets: u32,
        packet_size: u32,
        flash_offset: u32,
    ) -> Result<()> {
        self.send_command(Command::FlashBegin {
            total_size,
            num_packets,
            packet_size,
            flash_offset,
        })?;
        Ok(())
    }

    pub fn flash_data(&mut self, sequence_num: u32, data: &[u8]) -> Result<()> {
        self.send_command_with_data(
            Command::FlashData {
                sequence_num,
                data_size: data.len() as u32,
            },
            data,
        )?;
        Ok(())
    }

    pub fn flash_end(&mut self, reboot: bool) -> Result<()> {
        let reboot = if reboot { 0 } else { 1 };
        self.send_command(Command::FlashEnd { reboot })?;
        Ok(())
    }

    pub fn mem_begin(
        &mut self,
        total_size: u32,
        num_packets: u32,
        packet_size: u32,
        flash_offset: u32,
    ) -> Result<()> {
        self.send_command(Command::MemBegin {
            total_size,
            num_packets,
            packet_size,
            mem_offset: flash_offset,
        })?;
        Ok(())
    }

    pub fn mem_data(&mut self, sequence_num: u32, data: &[u8]) -> Result<()> {
        self.send_command_with_data(
            Command::MemData {
                data_size: data.len() as u32,
                sequence_num,
            },
            data,
        )?;
        Ok(())
    }

    pub fn mem_end(&mut self, execute: bool, entry_point: u32) -> Result<()> {
        self.send_command(Command::MemEnd {
            execute: if execute { 0 } else { 1 },
            entry_point,
        })?;
        Ok(())
    }

    pub fn flash_defl_begin(
        &mut self,
        total_size: u32,
        num_packets: u32,
        packet_size: u32,
        flash_offset: u32,
    ) -> Result<()> {
        self.send_command(Command::FlashDeflBegin {
            total_size,
            num_packets,
            packet_size,
            flash_offset,
        })?;
        Ok(())
    }

    pub fn flash_defl_data(&mut self, sequence_num: u32, data: &[u8]) -> Result<()> {
        let data_size = data.len().try_into().expect("data too long");
        self.send_command_with_data(
            Command::FlashDeflData {
                data_size,
                sequence_num,
            },
            data,
        )?;
        Ok(())
    }

    pub fn flash_defl_end(&mut self, reboot: bool) -> Result<()> {
        let reboot = if reboot { 0 } else { 1 };
        self.send_command(Command::FlashDeflEnd { reboot })?;
        Ok(())
    }

    pub fn sync(&mut self) -> Result<()> {
        let cmd = Command::Sync;
        let cmd_code = cmd.code();
        let timeout = cmd.timeout();
        self.send_command(cmd)?;

        for _ in 0..100 {
            match self.read_response(cmd_code, timeout) {
                Ok(_) => (),
                Err(err) if err.is_timeout() => return Ok(()),
                Err(err) => return Err(err),
            }
        }

        Ok(())
    }

    pub fn write_reg(&mut self, address: u32, value: u32, mask: u32, delay: u32) -> Result<()> {
        self.send_command(Command::WriteReg {
            address,
            value,
            mask,
            delay,
        })?;
        Ok(())
    }

    pub fn read_reg(&mut self, address: u32) -> Result<u32> {
        self.send_command(Command::ReadReg { address })
            .map(|(value, _data)| value)
    }

    pub fn spi_set_params(&mut self, total_size: u32) -> Result<()> {
        self.send_command(Command::SpiSetParams {
            id: 0,
            total_size,
            block_size: 0x10000,
            sector_size: 0x1000,
            page_size: 0x100,
            status_mask: 0xFFFF,
        })?;
        Ok(())
    }

    pub fn spi_attach(&mut self) -> Result<()> {
        self.send_command(Command::SpiAttach {
            pins: 0,
            rom_only: 0,
        })?;
        self.attached = true;
        Ok(())
    }

    pub fn change_baud_rate(&mut self, new_rate: u32) -> Result<()> {
        let old_rate = if self.rom_loader {
            0
        } else {
            self.serial().baud_rate()?
        };
        self.send_command(Command::ChangeBaudRate { new_rate, old_rate })?;
        self.serial.consume(self.serial.buffer().len());
        let serial = self.serial();
        serial.flush()?;
        serial.set_baud_rate(new_rate)?;
        Ok(())
    }

    pub fn spi_flash_md5(&mut self, address: u32, size: u32) -> Result<[u8; 16]> {
        let (_value, data) = self.send_command(Command::SpiFlashMD5 { address, size })?;
        let mut result = [0u8; 16];
        if self.rom_loader {
            if data.len() != 32 || !data.iter().all(u8::is_ascii_hexdigit) {
                return Err(CommandError::InvalidResponse.into());
            }
            let f = |x: u8| match x {
                b'0'..=b'9' => x - b'0',
                b'a'..=b'f' => x - b'a' + 10,
                b'A'..=b'F' => x - b'A' + 10,
                _ => unreachable!(),
            };
            for idx in 0..16 {
                result[idx] = 16 * f(data[2 * idx]) + f(data[2 * idx + 1]);
            }
        } else {
            if data.len() != 16 {
                return Err(CommandError::InvalidResponse.into());
            }
            for (idx, byte) in data.iter().enumerate() {
                result[idx] = *byte;
            }
        }
        Ok(result)
    }

    pub fn detect_chip(&mut self) -> Result<Chip> {
        let magic = self.read_reg(CHIP_MAGIC_REG)?;
        if let Some(chip) = Chip::try_from_magic(magic) {
            self.set_chip(chip);
            return Ok(chip);
        }
        Err(FlasherError::UnknownDevice(magic).into())
    }

    /// Write `data` to RAM at address `addr`. If `entry` is `Some(entry_point)`, then
    /// after receiving the data and writing it to ram, the loader will jump to the
    /// `entry_point` address.
    pub fn write_ram(&mut self, addr: u32, data: &[u8], entry: Option<u32>) -> Result<()> {
        let packet_size = min(data.len(), 0x4000);
        let total_size: u32 = data.len().try_into().unwrap();
        let num_packets = (total_size + packet_size as u32 - 1) / packet_size as u32;
        self.send_command(Command::MemBegin {
            total_size,
            num_packets,
            packet_size: packet_size as u32,
            mem_offset: addr,
        })?;

        for (num, chunk) in data.chunks(packet_size).enumerate() {
            let chunk = if chunk.len() == packet_size {
                Cow::Borrowed(chunk)
            } else {
                let mut owned: Vec<u8> = Vec::with_capacity(packet_size);
                owned.extend(chunk);
                owned.resize(packet_size, 0xFF);
                Cow::Owned(owned)
            };
            self.send_command_with_data(
                Command::MemData {
                    data_size: packet_size as u32,
                    sequence_num: num as u32,
                },
                &chunk,
            )?;
        }

        if let Some(entry) = entry {
            let ret = self.send_command(Command::MemEnd {
                execute: 0,
                entry_point: entry,
            });

            if !ret.is_timeout() {
                return ret.map(|_| ());
            }
        }
        Ok(())
    }

    // pub fn write_flash(&mut self, addr: u32, data: &[u8], compressed: bool, reboot: bool) -> Result<()> {

    // }

    pub fn run_stub(&mut self, stub: &[u8]) -> Result<()> {
        let stub = Stub::read(&mut Cursor::new(stub))?;
        let chip = stub
            .chip()
            .ok_or_else(|| Error::FormatError(format!("Unknown stub chip ID: {:X}", stub.chip)))?;
        let this_chip = self.chip()?;
        if chip != this_chip {
            return Err(Error::FormatError(format!(
                "Stub for {chip} not supported for {this_chip}"
            )));
        }
        self.write_ram(stub.text_start, &stub.text, None)?;
        self.write_ram(stub.data_start, &stub.data, Some(stub.entry))?;
        let ohai = self.read_packet(DEFAULT_SERIAL_TIMEOUT)?;

        if ohai != b"OHAI" {
            return Err(FlasherError::InvalidStubHello.into());
        }
        self.rom_loader = false;
        self.status_size = 2;
        Ok(())
    }
}
