use std::borrow::Cow;
use std::io::{self, Cursor};
use std::rc::Weak;
use std::time::{Duration, Instant};

use anyhow::Result;
use serialport::SerialPort;

use crate::command::{Command, CommandError};
use crate::event::{Event, EventObserver};
use crate::timeout::ErrorExt;

const DEFAULT_SERIAL_WRITE_TIMEOUT: Duration = Duration::from_millis(100);

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

pub struct Connection {
    serial: Box<dyn SerialPort>,
    buffer: Vec<u8>,
    rom_loader: bool,
    observers: Vec<Weak<dyn EventObserver>>,
}

impl Connection {
    pub fn new(path: &str) -> Result<Self> {
        let serial = serialport::new(path, 115200).open()?;
        Ok(Connection {
            serial,
            buffer: Vec::with_capacity(1024),
            rom_loader: true,
            observers: Vec::new(),
        })
    }

    pub fn add_observer<E>(&mut self, observer: Weak<E>)
    where
        E: EventObserver + 'static,
    {
        self.observers.push(observer);
    }

    fn send(&mut self, cmd: Command) -> Result<(u32, Vec<u8>)> {
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
                for _ in 0..32 {
                    data.push(0x55);
                }
            }
            Command::ReadReg { address } => {
                data.extend(address.to_le_bytes());
            }
            _ => unimplemented!(),
        }

        let timeout = cmd.timeout();
        let len: u16 = (data.len() - 8).try_into()?;
        data[2] = len as u8;
        data[3] = (len >> 8) as u8;
        data[4] = checksum;

        self.trace(Event::Command(cmd));

        let mut encoder = slip_codec::SlipEncoder::new(true);
        let mut output: Vec<u8> = Vec::with_capacity(data.len() + 8);

        encoder.encode(&data, &mut output)?;
        self.trace(Event::SlipWrite(Cow::from(&data)));

        self.serial.set_timeout(DEFAULT_SERIAL_WRITE_TIMEOUT)?;
        self.serial.write_all(&output)?;
        self.trace(Event::SerialWrite(Cow::from(&output)));

        self.read_response(cmd_code, timeout)
    }

    fn read_response(&mut self, cmd_code: u8, timeout: Duration) -> Result<(u32, Vec<u8>)> {
        let now = Instant::now();
        let mut decoder = slip_codec::SlipDecoder::new();
        let mut response: Vec<u8> = Vec::new();
        let status_size = if self.rom_loader { 4 } else { 2 };

        loop {
            let remaining = timeout
                .checked_sub(now.elapsed())
                .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "Command timeout"))?;

            if self.buffer.is_empty() {
                self.fill_buffer(remaining)?;
            }

            let mut cursor = Cursor::new(&self.buffer);
            match decoder.decode(&mut cursor, &mut response) {
                Ok(_) => {
                    // A complete packet has been decoded.
                    let len = cursor.position() as usize;
                    self.trace(Event::SlipRead(Cow::from(&response)));
                    self.buffer.drain(..len);
                    if response.len() < 10 || response[0] != 1 || response[1] != cmd_code || {
                        let size = from_le16(&response[2..4]) as usize;
                        size + 8 > response.len() || size < status_size
                    } {
                        decoder = slip_codec::SlipDecoder::new();
                        continue;
                    }
                    break;
                }
                Err(slip_codec::SlipError::EndOfStream) => {
                    // The decoder did not hit the end of the packet but did consume all of self.buffer.
                    self.buffer.clear();
                }
                Err(_) => {
                    // XXX: Trace something here?
                    return Err(CommandError::InvalidResponse.into());
                }
            }
        }

        // Check if the response is an error.
        match response[response.len() - status_size] {
            0 => {
                let value = from_le32(&response[4..8]);
                let data = response[8..response.len() - status_size].to_vec();
                self.trace(Event::Response(value, Cow::from(&data)));
                Ok((value, data))
            }
            1 => {
                let err = CommandError::from(response[response.len() - status_size + 1]);
                self.trace(Event::Error(err.clone()));
                Err(err.into())
            }
            _ => {
                self.trace(Event::Error(CommandError::InvalidResponse));
                Err(CommandError::InvalidResponse.into())
            }
        }
    }

    pub fn read_line(&mut self, timeout: Duration) -> Result<Vec<u8>> {
        let mut line: Vec<u8> = Vec::new();
        if self.buffer.is_empty() {
            self.fill_buffer(timeout)?;
        }

        loop {
            if let Some(idx) = self.buffer.iter().position(|&x| x == b'\n') {
                line.extend(self.buffer.drain(..idx + 1));
                self.trace(Event::SerialLine(Cow::from(&line)));
                return Ok(line);
            }
            line.append(&mut self.buffer);
            self.fill_buffer(timeout)?;
        }
    }

    fn fill_buffer(&mut self, timeout: Duration) -> Result<()> {
        self.buffer.resize(1024, 0);
        self.serial.set_timeout(timeout)?;
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
        self.reset()?;
        // Look for boot message.
        let mut waiting = false;
        for _ in 0..10 {
            let line = self.read_line(timeout)?;
            if line == b"waiting for download\r\n" {
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

    pub fn reset(&mut self) -> Result<()> {
        // /RTS is connected to EN
        // /DTR is connected to GPIO0
        self.serial.write_request_to_send(true)?;
        self.serial.write_data_terminal_ready(false)?;
        std::thread::sleep(Duration::from_millis(100));
        self.serial.write_data_terminal_ready(true)?;
        self.serial.write_request_to_send(false)?;

        self.trace(Event::Reset);
        self.buffer.clear();
        self.serial.flush()?;
        Ok(())
    }

    pub fn sync(&mut self) -> Result<()> {
        let cmd = Command::Sync;
        let cmd_code = cmd.code();
        let timeout = cmd.timeout();
        self.send(cmd)?;

        for _ in 0..100 {
            match self.read_response(cmd_code, timeout) {
                Ok(_) => (),
                Err(err) => {
                    if let Some(err) = err.downcast_ref::<io::Error>() {
                        if err.kind() == io::ErrorKind::TimedOut {
                            return Ok(());
                        }
                    }
                }
            }
        }

        Ok(())
    }

    pub fn read_reg(&mut self, address: u32) -> Result<u32> {
        self.send(Command::ReadReg { address })
            .map(|(value, _data)| value)
    }
}
