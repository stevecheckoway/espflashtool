// Copyright 2022 Stephen Checkoway
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::borrow::Cow;
use std::cmp::max;
use std::io::{self, BufRead, BufReader, Cursor};
use std::rc::Rc;
use std::time::{Duration, Instant};

use binrw::{BinRead, BinWrite};
use serialport::SerialPort;

use crate::command::{Command, CommandError, ResponsePacket};
use crate::event::{Event, EventObserver, EventProvider};
use crate::timeout::ErrorExt;
use crate::Error;
use crate::Result;

const DEFAULT_SERIAL_TIMEOUT: Duration = Duration::from_millis(10);

struct TimeoutSerialPort {
    inner: Box<dyn SerialPort>,
    start: Instant,
    timeout: Duration,
    event_provider: EventProvider,
}

impl std::io::Read for TimeoutSerialPort {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let timeout = max(
            DEFAULT_SERIAL_TIMEOUT,
            self.timeout.saturating_sub(self.start.elapsed()),
        );
        self.inner.set_timeout(timeout)?;
        let size = self.inner.read(buf)?;
        self.event_provider
            .send_event(Event::SerialRead(Cow::from(&buf[..size])));
        Ok(size)
    }
}

pub struct Protocol {
    serial: BufReader<TimeoutSerialPort>,
    is_rom_loader: bool,
    event_provider: EventProvider,
}

impl Protocol {
    pub(crate) fn new(serial: Box<dyn SerialPort>) -> Self {
        let event_provider = EventProvider::new();
        let serial = TimeoutSerialPort {
            inner: serial,
            start: Instant::now(),
            timeout: DEFAULT_SERIAL_TIMEOUT,
            event_provider: event_provider.clone(),
        };

        Protocol {
            serial: BufReader::new(serial),
            is_rom_loader: true,
            event_provider: EventProvider::new(),
        }
    }

    pub fn add_observer<O>(&mut self, observer: O)
    where
        O: Into<Rc<dyn EventObserver + 'static>>,
    {
        self.event_provider.add_observer(observer.into());
    }

    pub fn remove_observer<O>(&mut self, observer: O)
    where
        O: AsRef<Rc<dyn EventObserver + 'static>>,
    {
        self.event_provider.remove_observer(observer.as_ref());
    }

    pub fn is_rom_loader(&self) -> bool {
        self.is_rom_loader
    }

    pub fn set_rom_loader(&mut self, is_rom_loader: bool) {
        self.is_rom_loader = is_rom_loader;
    }

    #[inline]
    fn serial(&mut self) -> &mut dyn SerialPort {
        self.serial.get_mut().inner.as_mut()
    }

    #[inline]
    fn set_timeout(&mut self, timeout: Duration) {
        let tsp = self.serial.get_mut();
        tsp.start = Instant::now();
        tsp.timeout = timeout;
    }

    fn send_packet(&mut self, data: &[u8]) -> Result<()> {
        let mut encoder = slip_codec::SlipEncoder::new(true);
        let mut output: Vec<u8> = Vec::with_capacity(data.len() + 8);

        encoder.encode(data, &mut output)?;
        // Trace the SlipWrite after performing it because the encoder is
        // extremely unlikely to fail when reading and write to memory and
        // this lets the Cow own the data, potentially saving a copy later.
        //
        // In contrast, writing to the serial port is likely to fail. It's
        // better to trace before the write happens to make debugging
        // easier. We lose out on potentially saving a copy though.
        self.trace(Event::SlipWrite(Cow::from(data)));
        self.trace(Event::SerialWrite(Cow::from(&output)));

        let serial = self.serial();
        serial.set_timeout(DEFAULT_SERIAL_TIMEOUT)?;
        serial.write_all(&output)?;
        Ok(())
    }

    pub fn read_packet(&mut self, timeout: Duration) -> Result<Vec<u8>> {
        let mut response: Vec<u8> = Vec::new();
        let mut decoder = slip_codec::SlipDecoder::new();

        self.set_timeout(timeout);
        decoder
            .decode(&mut self.serial, &mut response)
            .map_err(|err| Error::IOError(err.into()))?;
        self.trace(Event::SlipRead(Cow::from(&response)));
        Ok(response)
    }

    #[inline]
    fn send_command(&mut self, cmd: Command) -> Result<(u32, Vec<u8>)> {
        self.send_command_with_data(cmd, &[])
    }

    fn send_command_with_data(&mut self, cmd: Command, data: &[u8]) -> Result<(u32, Vec<u8>)> {
        let mut packet: Vec<u8> = Vec::with_capacity(64);
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
        packet.extend(&[0, cmd_code, 0, 0, checksum, 0, 0, 0]);
        {
            let mut cursor = Cursor::new(&mut packet);
            cursor.set_position(8);
            cmd.write_with_args(&mut cursor, (self.is_rom_loader,))?;
        }
        packet.extend(data);

        let len: u16 = (packet.len() - 8).try_into().expect("Data too long");
        packet[2] = len as u8;
        packet[3] = (len >> 8) as u8;
        packet[4] = checksum;

        let timeout = cmd.timeout();
        self.trace(Event::Command(cmd, Cow::Borrowed(data)));
        self.send_packet(&packet)?;
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

        loop {
            let response = self.read_packet(timeout.saturating_sub(start_time.elapsed()))?;
            let mut cursor = Cursor::new(&response);
            match ResponsePacket::read(&mut cursor) {
                Err(_) => self.trace(Event::InvalidResponse(Cow::from(response))),
                Ok(ResponsePacket {
                    cmd_code: cmd,
                    value,
                    data,
                    status,
                    error,
                    ..
                }) => {
                    self.trace(Event::Response(cmd, status, error, value, Cow::from(&data)));

                    if cmd == cmd_code {
                        match status {
                            0 => return Ok((value, data.to_vec())),
                            1 => return Err(CommandError::from(error).into()),
                            _ => return Err(CommandError::InvalidResponse.into()),
                        }
                    }
                }
            }
        }
    }

    // Read a line of text.
    // If a complete line has not been received by the timeout, then subsequent
    // bytes must arrive within `DEFAULT_SERIAL_TIMEOUT`.
    pub fn read_line(&mut self, timeout: Duration) -> Result<Vec<u8>> {
        let mut line: Vec<u8> = Vec::new();

        self.set_timeout(timeout);
        self.serial.read_until(b'\n', &mut line)?;
        self.trace(Event::SerialLine(Cow::from(&line)));
        Ok(line)
    }

    #[inline]
    fn trace(&mut self, event: Event) {
        self.event_provider.send_event(event);
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

    pub fn reset(&mut self, enter_bootloader: bool) -> Result<()> {
        self.trace(Event::Reset);
        self.serial.consume(self.serial.buffer().len());

        self.is_rom_loader = true;

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
        erase_size: u32,
        num_packets: u32,
        packet_size: u32,
        flash_offset: u32,
    ) -> Result<()> {
        self.send_command(Command::FlashBegin {
            erase_size,
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
        mem_offset: u32,
    ) -> Result<()> {
        self.send_command(Command::MemBegin {
            total_size,
            num_packets,
            packet_size,
            mem_offset,
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
        erase_size: u32,
        num_packets: u32,
        packet_size: u32,
        flash_offset: u32,
    ) -> Result<()> {
        self.send_command(Command::FlashDeflBegin {
            erase_size,
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

    pub fn write_reg(&mut self, address: u32, value: u32) -> Result<()> {
        self.send_command(Command::WriteReg {
            address,
            value,
            mask: 0xFFFFFFFF,
            delay: 0,
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
        Ok(())
    }

    pub fn change_baud_rate(&mut self, new_rate: u32) -> Result<()> {
        let old_rate = if self.is_rom_loader {
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
        if self.is_rom_loader {
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
}
