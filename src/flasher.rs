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
use std::cmp::{max, min};
use std::io::{self, Cursor, Write};
use std::rc::Rc;
use std::time::{Duration, Instant};

use binrw::BinRead;
use flate2::write::DeflateEncoder;
use flate2::Compression;
use serialport::SerialPort;

use crate::chip::Chip;
use crate::event::{Event, EventObserver, EventProvider};
use crate::protocol::Protocol;
use crate::stub::Stub;
use crate::timeout::ErrorExt;
use crate::Result;
use crate::{from_be16, from_le, Error};

const DEFAULT_SERIAL_TIMEOUT: Duration = Duration::from_millis(10);

const MEM_PACKET_SIZE: usize = 0x1800; // 6 kB
const FLASH_SECTOR_SIZE: usize = 0x1000; //  4 kB
const ROM_PACKET_SIZE: usize = 0x400; //  1 kB
const STUB_PACKET_SIZE: usize = 0x4000; // 16 kB
const DATA_SIZE_MULTIPLE: usize = 4;

const CHIP_MAGIC_REG: u32 = 0x40001000;

#[derive(Clone, Copy, Debug, thiserror::Error)]
pub enum FlasherError {
    #[error("Unknown ESP device ({:08X})", .0)]
    UnknownDevice(u32),

    #[error("Invalid SPI command or address length")]
    InvalidSpiCommand,

    #[error("Invalid stub hello")]
    InvalidStubHello,

    #[error("Flash offset not a multiple of 4096")]
    MisalignedFlashOffset,

    #[error("Stub loader already running")]
    StubAlreadyRunning,
}

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

pub struct Flasher {
    protocol: Protocol,
    chip: Option<Chip>,
    attached: bool,
}

impl Flasher {
    pub fn new(path: &str) -> Result<Self> {
        let serial = serialport::new(path, 115200).open()?;

        Ok(Flasher {
            protocol: Protocol::new(serial),
            chip: None,
            attached: false,
        })
    }

    pub fn add_observer<O>(&mut self, observer: O)
    where
        O: Into<Rc<dyn EventObserver + 'static>>,
    {
        self.protocol.add_observer(observer);
    }

    pub fn remove_observer<O>(&mut self, observer: O)
    where
        O: AsRef<Rc<dyn EventObserver + 'static>>,
    {
        self.protocol.remove_observer(observer);
    }

    pub fn connect(&mut self) -> Result<Chip> {
        self.attached = false;
        self.protocol.connect()?;
        let magic = self.protocol.read_reg(CHIP_MAGIC_REG)?;
        self.chip = Chip::try_from_magic(magic);
        self.chip
            .ok_or_else(|| FlasherError::UnknownDevice(magic).into())
    }

    fn ensure_connected(&mut self) -> Result<Chip> {
        self.chip.ok_or(()).or_else(|_err| self.connect())
    }

    fn ensure_attached(&mut self) -> Result<()> {
        let chip = self.ensure_connected()?;
        if !self.attached {
            if chip == Chip::Esp8266 {
                self.protocol.flash_begin(0, 0, 0, 0)?;
            } else {
                self.protocol.spi_attach()?;
            }
            self.attached = true;
        }
        Ok(())
    }

    pub fn change_baud_rate(&mut self, new_rate: u32) -> Result<()> {
        self.ensure_connected()?;
        self.protocol.change_baud_rate(new_rate)
    }

    #[inline]
    pub fn chip(&mut self) -> Result<Chip> {
        self.ensure_connected()
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
        let chip = self.ensure_connected()?;
        self.ensure_attached()?;

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
        self.protocol
            .write_reg(regs.user2, user2_data, 0xFFFFFFFF, 0)?;

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
            self.protocol.write_reg(regs.addr, address, 0xFFFFFFFF, 0)?;
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
                self.protocol
                    .write_reg(regs.mosi_dlen, data_len, 0xFFFFFFFF, 0)?;
            }

            for (pos, val) in data.chunks(4).enumerate() {
                let val = from_le(val);
                self.protocol.write_reg(regs.w(pos), val, 0xFFFFFFFF, 0)?;
            }
        }
        if !output.is_empty() {
            user_data |= SPI_USR_MISO;
            let output_len = (output.len() * 8 - 1) as u32;
            if chip == Chip::Esp8266 {
                user1_data |= output_len << 8;
            } else {
                self.protocol
                    .write_reg(regs.miso_dlen, output_len, 0xFFFFFFFF, 0)?;
            }
        }
        self.protocol
            .write_reg(regs.user1, user1_data, 0xFFFFFFFF, 0)?;
        self.protocol
            .write_reg(regs.user, user_data, 0xFFFFFFFF, 0)?;
        self.protocol.write_reg(regs.cmd, SPI_USR, 0xFFFFFFFF, 0)?;

        loop {
            let cmd = self.protocol.read_reg(regs.cmd)?;
            if cmd & SPI_USR == 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }

        // Read output.
        for (pos, output_val) in output.chunks_mut(4).enumerate() {
            let val = self.protocol.read_reg(regs.w(pos))?.to_le_bytes();
            output_val.copy_from_slice(&val[..output_val.len()]);
        }
        Ok(())
    }

    pub fn reset(&mut self, enter_bootloader: bool) -> Result<()> {
        self.attached = false;
        self.chip = None;
        self.protocol.reset(enter_bootloader)
    }

    fn write_all_data(
        &mut self,
        data: &[u8],
        packet_size: usize,
        pad_last: bool,
        data_fn: fn(&mut Protocol, u32, &[u8]) -> Result<()>,
    ) -> Result<()> {
        for (sequence_num, chunk) in data.chunks(packet_size).enumerate() {
            if !pad_last || chunk.len() == packet_size {
                data_fn(&mut self.protocol, sequence_num as u32, chunk)?;
            } else {
                let mut last_chunk = chunk.to_vec();
                last_chunk.resize(packet_size, 0xFF);
                data_fn(&mut self.protocol, sequence_num as u32, &last_chunk)?;
            }
        }
        Ok(())
    }

    /// Write `data` to RAM at address `addr`. If `entry` is `Some(entry_point)`, then
    /// after receiving the data and writing it to ram, the loader will jump to the
    /// `entry_point` address.
    pub fn write_ram(&mut self, addr: u32, data: &[u8], entry: Option<u32>) -> Result<()> {
        self.ensure_connected()?;
        let packet_size = min(data.len(), MEM_PACKET_SIZE);
        let total_size: u32 = data.len().try_into().unwrap();
        let num_packets = (total_size + packet_size as u32 - 1) / packet_size as u32;
        self.protocol
            .mem_begin(total_size, num_packets, packet_size as u32, addr)?;
        self.write_all_data(data, packet_size, true, Protocol::mem_data)?;

        if let Some(entry) = entry {
            // The ROM loader may start executing the code before the
            // transmit fifo is empty, so ignore timeouts.
            let ret = self.protocol.mem_end(true, entry);

            if !self.protocol.is_rom_loader() || !ret.is_timeout() {
                return ret;
            }
        }
        Ok(())
    }

    pub fn write_flash(
        &mut self,
        flash_offset: u32,
        data: &[u8],
        compress: bool,
        reboot: bool,
    ) -> Result<()> {
        if flash_offset as usize & (FLASH_SECTOR_SIZE - 1) != 0 {
            return Err(FlasherError::MisalignedFlashOffset.into());
        }
        let chip = self.ensure_connected()?;

        let packet_size = if self.protocol.is_rom_loader() {
            ROM_PACKET_SIZE
        } else {
            STUB_PACKET_SIZE
        };

        let mask = DATA_SIZE_MULTIPLE - 1;
        let padded_size = (data.len() + mask) & !mask;
        let padding_size = padded_size - data.len();
        let erase_size = if chip == Chip::Esp8266 {
            todo!("ESP8266 has some bizarre erase bug");
        } else {
            padded_size as u32
        };

        if compress {
            // Compress the data and the padding bytes.
            let mut e = DeflateEncoder::new(Vec::new(), Compression::best());
            e.write_all(data)?;
            if padding_size > 0 {
                let mut padding = Vec::with_capacity(padding_size);
                padding.resize(padding_size, 0xFF);
                e.write_all(&padding)?;
            }
            let compressed_data: Vec<u8> = e.finish()?;
            let num_packets = ((compressed_data.len() + (packet_size - 1)) / packet_size) as u32;

            // Send the FLASH_DEFL_BEGIN and FLASH_DEFL_DATA packets.
            self.protocol.flash_defl_begin(
                erase_size,
                num_packets,
                packet_size as u32,
                flash_offset,
            )?;
            self.write_all_data(
                &compressed_data,
                packet_size,
                false,
                Protocol::flash_defl_data,
            )?;
        } else {
            // Pad the final packet to packet_size.
            let padded_size = (padded_size + packet_size - 1) & !(packet_size - 1);
            let num_packets = (padded_size / packet_size) as u32;
            self.protocol
                .flash_begin(erase_size, num_packets, packet_size as u32, flash_offset)?;
            self.write_all_data(data, packet_size, true, Protocol::flash_data)?;
        }

        match (reboot, compress) {
            (true, true) => self.protocol.flash_defl_end(reboot),
            (true, false) => self.protocol.flash_end(reboot),
            (false, _) => Ok(()),
        }
    }

    pub fn run_stub(&mut self, stub: &[u8]) -> Result<()> {
        let this_chip = self.ensure_connected()?;
        if !self.protocol.is_rom_loader() {
            return Err(FlasherError::StubAlreadyRunning.into());
        }
        let stub = Stub::read(&mut Cursor::new(stub))?;
        let chip = stub
            .chip()
            .ok_or_else(|| Error::FormatError(format!("Unknown stub chip ID: {:X}", stub.chip)))?;
        if chip != this_chip {
            return Err(Error::FormatError(format!(
                "Stub for {chip} not supported for {this_chip}"
            )));
        }
        self.write_ram(stub.text_start, &stub.text, None)?;
        self.write_ram(stub.data_start, &stub.data, Some(stub.entry))?;
        let ohai = self.protocol.read_packet(DEFAULT_SERIAL_TIMEOUT)?;

        if ohai != b"OHAI" {
            return Err(FlasherError::InvalidStubHello.into());
        }
        self.protocol.set_rom_loader(false);
        Ok(())
    }
}
