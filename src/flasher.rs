use std::borrow::Cow;
use std::cmp::max;
use std::io::{self, Cursor};
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serialport::SerialPort;

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
fn from_le32(data: &[u8]) -> u32 {
    let data: [u8; 4] = [data[0], data[1], data[2], data[3]];
    u32::from_le_bytes(data)
}

#[derive(Clone, Copy, Debug, thiserror::Error)]
pub enum FlasherError {
    #[error("Unknown ESP device ({:08X})", .0)]
    UnknownDevice(u32),

    #[error("Command cannot be sent without setting or detecting chip first")]
    MustSetChipFirst
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Chip {
    Esp8266,
    Esp32,
    Esp32S2,
    Esp32S3,
    Esp32C3,
}

impl Chip {
    pub fn try_from_magic(magic: u32) -> Option<Self> {
        // https://github.com/espressif/esp-serial-flasher/blob/master/src/esp_targets.c
        match magic {
            0xFFF0C101 => Some(Chip::Esp8266),
            0x00F01D83 => Some(Chip::Esp32),
            0x000007c6 => Some(Chip::Esp32S2),
            0x6921506F | 0x1B31506F => Some(Chip::Esp32C3),
            0x00000009 => Some(Chip::Esp32S3),
            _ => None,
        }
    }
}

pub struct Flasher {
    serial: Box<dyn SerialPort>,
    buffer: Vec<u8>,
    status_size: usize,
    rom_loader: bool,
    observers: Vec<Weak<dyn EventObserver>>,
    owned_observers: Vec<Rc<dyn EventObserver>>,
    chip: Option<Chip>,
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

    pub fn chip(&self) -> Option<Chip> {
        self.chip
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

    fn send_command(&mut self, cmd: Command) -> Result<(u32, Vec<u8>)> {
        let mut data: Vec<u8> = Vec::with_capacity(64);
        let checksum: u8 = 0;
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
        data.extend(&[0, cmd_code, 0, 0, 0, 0, 0, 0]);

        match cmd {
            Command::Sync => {
                data.extend(&[0x07, 0x07, 0x12, 0x20]);
                data.resize(data.len() + 32, 0x55);
            }
            Command::ReadReg { address } => {
                data.extend(address.to_le_bytes());
            }
            Command::ChangeBaudRate { new_rate } => {
                let old_rate = if self.rom_loader { 0 } else { self.serial.baud_rate()? };
                data.extend(new_rate.to_le_bytes());
                data.extend(old_rate.to_le_bytes());
            }
            _ => unimplemented!(),
        }

        let len: u16 = (data.len() - 8).try_into()?;
        data[2] = len as u8;
        data[3] = (len >> 8) as u8;
        data[4] = checksum;

        let timeout = cmd.timeout();
        self.trace(Event::Command(cmd));
        self.send_packet(&data)?;
        self.read_response(cmd_code, timeout)
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
        self.serial.set_timeout(max(DEFAULT_SERIAL_TIMEOUT, timeout))?;
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
        self.reset(true)?;
        // Look for boot message.
        let mut waiting = false;
        for _ in 0..10 {
            let line = self.read_line(timeout);
            if line.is_timeout() {
                // XXX: Does the ESP8266 write this message but just at a different baud rate?
                waiting = true;
                break;
            }
            if line? == b"waiting for download\r\n" {
                waiting = true;
                break;
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
        // /RTS is connected to EN
        // /DTR is connected to GPIO0
        self.serial.write_request_to_send(true)?;
        self.serial.write_data_terminal_ready(false)?;
        std::thread::sleep(Duration::from_millis(100));

        self.serial.write_data_terminal_ready(enter_bootloader)?;
        self.serial.write_request_to_send(false)?;
        std::thread::sleep(Duration::from_millis(500));
        self.serial.write_data_terminal_ready(false)?;

        self.trace(Event::Reset);
        self.buffer.clear();
        self.serial.flush()?;
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

    pub fn read_reg(&mut self, address: u32) -> Result<u32> {
        self.send_command(Command::ReadReg { address })
            .map(|(value, _data)| value)
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

    pub fn detect_chip(&mut self) -> Result<Chip> {
        let magic = self.read_reg(CHIP_MAGIC_REG)?;
        if let Some(chip) = Chip::try_from_magic(magic) {
            self.set_chip(chip);
            return Ok(chip);
        }
        Err(FlasherError::UnknownDevice(magic).into())
    }
}
