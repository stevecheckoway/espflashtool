use std::borrow::Cow;
use std::cmp::max;
use std::io::{self, Cursor};
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serialport::SerialPort;

use crate::chip::Chip;
use crate::command::{Command, CommandError};
use crate::event::{Event, EventObserver};
use crate::timeout::ErrorExt;

const DEFAULT_SERIAL_TIMEOUT: Duration = Duration::from_millis(10);

const CHIP_MAGIC_REG: u32 = 0x40001000;

#[inline]
fn from_le16(data: &[u8]) -> u16 {
    let data: [u8; 2] = [data[0], data[1]];
    u16::from_le_bytes(data)
}

#[inline]
fn from_le24(data: &[u8]) -> u32 {
    let data: [u8; 4] = [data[0], data[1], data[2], 0];
    u32::from_le_bytes(data)
}

#[inline]
fn from_le32(data: &[u8]) -> u32 {
    let data: [u8; 4] = [data[0], data[1], data[2], data[3]];
    u32::from_le_bytes(data)
}

#[inline]
fn from_le(data: &[u8]) -> u32 {
    assert!(data.len() <= 4);
    let mut le_data = [0u8; 4];
    (&mut le_data[..data.len()]).copy_from_slice(data);
    u32::from_le_bytes(le_data)
}

#[inline]
fn from_be16(data: &[u8]) -> u16 {
    u16::from_be_bytes([data[0], data[1]])
}

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
}

pub struct Flasher {
    serial: Box<dyn SerialPort>,
    buffer: Vec<u8>,
    status_size: usize,
    rom_loader: bool,
    observers: Vec<Weak<dyn EventObserver>>,
    owned_observers: Vec<Rc<dyn EventObserver>>,
    chip: Option<Chip>,
    attached: bool,
}

impl Flasher {
    pub fn new(path: &str) -> Result<Self> {
        let serial = serialport::new(path, 115200)
            .open()
            .with_context(|| format!("Failed to open {}", path))?;
        Ok(Flasher {
            serial,
            buffer: Vec::with_capacity(1024),
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

    fn send_packet(&mut self, data: &[u8]) -> Result<()> {
        let mut encoder = slip_codec::SlipEncoder::new(true);
        let mut output: Vec<u8> = Vec::with_capacity(data.len() + 8);

        encoder.encode(data, &mut output)?;
        self.trace(Event::SlipWrite(Cow::from(data)));

        self.serial.set_timeout(DEFAULT_SERIAL_TIMEOUT)?;
        self.serial.write_all(&output)?;
        self.trace(Event::SerialWrite(Cow::from(&output)));
        Ok(())
    }

    fn read_packet(&mut self, timeout: Duration) -> Result<Vec<u8>> {
        let start_time = Instant::now();
        let mut decoder = slip_codec::SlipDecoder::new();
        let mut response: Vec<u8> = Vec::new();

        loop {
            if self.buffer.is_empty() {
                self.fill_buffer(timeout.saturating_sub(start_time.elapsed()))?;
            }

            let mut cursor = Cursor::new(&self.buffer);
            match decoder.decode(&mut cursor, &mut response) {
                Ok(_) => {
                    // A complete packet has been decoded.
                    let size = cursor.position() as usize;
                    self.buffer.drain(..size);
                    self.trace(Event::SlipRead(Cow::from(&response)));
                    return Ok(response);
                }
                Err(slip_codec::SlipError::EndOfStream) => {
                    // The decoder did not hit the end of the packet but did consume all of self.buffer.
                    self.buffer.clear();
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
        let add_words = |v: &mut Vec<u8>, words: &[u32]| {
            for word in words {
                v.extend(word.to_le_bytes());
            }
        };
        let mut slip_data: Vec<u8> = Vec::with_capacity(64);
        let mut checksum: u8 = 0;
        let cmd_code = cmd.code();

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
        slip_data.extend(&[0, cmd_code, 0, 0, 0, 0, 0, 0]);

        match cmd {
            Command::FlashBegin {
                total_size,
                num_packets,
                packet_size,
                flash_offset,
            }
            | Command::MemBegin {
                total_size,
                num_packets,
                packet_size,
                flash_offset,
            }
            | Command::FlashDeflBegin {
                total_size,
                num_packets,
                packet_size,
                flash_offset,
            } => {
                add_words(
                    &mut slip_data,
                    &[total_size, num_packets, packet_size, flash_offset],
                );
            }
            Command::FlashData { sequence_num }
            | Command::MemData { sequence_num }
            | Command::FlashDeflData { sequence_num } => {
                add_words(&mut slip_data, &[data.len() as u32, sequence_num, 0, 0]);
                checksum = 0xEFu8;
                for x in data {
                    checksum ^= *x;
                }
                slip_data.extend(data);
            }
            Command::FlashEnd { reboot } | Command::FlashDeflEnd { reboot } => {
                let reboot_or_run_user_code = if reboot { 0 } else { 1 };
                add_words(&mut slip_data, &[reboot_or_run_user_code])
            }
            Command::MemEnd {
                execute,
                entry_point,
            } => {
                let val = if execute { 0 } else { 1 };
                add_words(&mut slip_data, &[val, entry_point]);
            }
            Command::Sync => {
                slip_data.extend(&[0x07, 0x07, 0x12, 0x20]);
                slip_data.resize(slip_data.len() + 32, 0x55);
            }
            Command::WriteReg {
                address,
                value,
                mask,
                delay,
            } => {
                add_words(&mut slip_data, &[address, value, mask, delay]);
            }
            Command::ReadReg { address } => {
                add_words(&mut slip_data, &[address]);
            }
            Command::SpiSetParams { size } => {
                // id, total size, block size, sector size, page size, status mask
                add_words(&mut slip_data, &[0, size, 0x10000, 0x1000, 0x100, 0xFFFF]);
            }
            Command::SpiAttach { pins } => {
                if self.rom_loader {
                    add_words(&mut slip_data, &[pins, 0]);
                } else {
                    add_words(&mut slip_data, &[pins]);
                }
            }
            Command::ChangeBaudRate { new_rate } => {
                let old_rate = if self.rom_loader {
                    0
                } else {
                    self.serial.baud_rate()?
                };
                add_words(&mut slip_data, &[new_rate, old_rate]);
            }
            Command::SpiFlashMD5 { address, size } => {
                add_words(&mut slip_data, &[address, size, 0, 0]);
            }
            Command::EraseFlash => {}
            Command::EraseRegion { offset, size } => {
                add_words(&mut slip_data, &[offset, size]);
            }
            Command::ReadFlash {
                offset,
                read_length,
                packet_size,
                max_pending_packets,
            } => {
                add_words(
                    &mut slip_data,
                    &[offset, read_length, packet_size, max_pending_packets],
                );
            }
            Command::RunUserCode => {}
        }

        let len: u16 = (slip_data.len() - 8).try_into()?;
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

        loop {
            if self.buffer.is_empty() {
                self.fill_buffer(timeout.saturating_sub(read_start.elapsed()))?;
            }

            if let Some(idx) = self.buffer.iter().position(|&x| x == b'\n') {
                line.extend(self.buffer.drain(..idx + 1));
                self.trace(Event::SerialLine(Cow::from(&line)));
                return Ok(line);
            }
            line.append(&mut self.buffer);
        }
    }

    fn fill_buffer(&mut self, timeout: Duration) -> Result<()> {
        self.buffer.resize(1024, 0);
        self.serial
            .set_timeout(max(DEFAULT_SERIAL_TIMEOUT, timeout))?;
        match self.serial.read(&mut self.buffer) {
            Ok(n) => {
                self.buffer.truncate(n);
                self.trace_only(Event::SerialRead(Cow::from(&self.buffer)));
                Ok(())
            }
            Err(err) => {
                // XXX: Trace?
                self.buffer.clear();
                Err(err.into())
            }
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
            self.spi_attach()
                .context("Failed to attach to the SPI flash")?;
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
        self.buffer.clear();

        // /RTS is connected to EN
        // /DTR is connected to GPIO0
        self.serial.write_request_to_send(true)?;
        self.serial.write_data_terminal_ready(false)?;
        std::thread::sleep(Duration::from_millis(100));
        self.serial.clear(serialport::ClearBuffer::All)?;

        self.serial.write_data_terminal_ready(enter_bootloader)?;
        self.serial.write_request_to_send(false)?;
        std::thread::sleep(Duration::from_millis(500));
        self.serial.write_data_terminal_ready(false)?;

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
        self.send_command_with_data(Command::FlashData { sequence_num }, data)?;
        Ok(())
    }

    pub fn flash_end(&mut self, reboot: bool) -> Result<()> {
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
            flash_offset,
        })?;
        Ok(())
    }

    pub fn mem_data(&mut self, sequence_num: u32, data: &[u8]) -> Result<()> {
        self.send_command_with_data(Command::MemData { sequence_num }, data)?;
        Ok(())
    }

    pub fn mem_end(&mut self, execute: bool, entry_point: u32) -> Result<()> {
        self.send_command(Command::MemEnd {
            execute,
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
        self.send_command_with_data(Command::FlashDeflData { sequence_num }, data)?;
        Ok(())
    }

    pub fn flash_defl_end(&mut self, reboot: bool) -> Result<()> {
        self.send_command(Command::FlashDeflEnd { reboot })?;
        Ok(())
    }

    pub fn sync(&mut self) -> Result<()> {
        let cmd = Command::Sync;
        let cmd_code = cmd.code();
        let timeout = cmd.timeout();
        self.send_command(cmd)
            .context("Timed out waiting for response to Sync command")?;

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

    pub fn spi_set_params(&mut self, size: u32) -> Result<()> {
        self.send_command(Command::SpiSetParams { size })?;
        Ok(())
    }

    pub fn spi_attach(&mut self) -> Result<()> {
        self.send_command(Command::SpiAttach { pins: 0 })?;
        self.attached = true;
        Ok(())
    }

    pub fn change_baud_rate(&mut self, new_rate: u32) -> Result<()> {
        self.send_command(Command::ChangeBaudRate { new_rate })?;
        self.buffer.clear();
        self.serial.set_baud_rate(new_rate)?;
        while self.serial.bytes_to_read()? > 0 {
            self.serial.clear(serialport::ClearBuffer::All)?;
            std::thread::sleep(Duration::from_millis(5));
        }
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
}
