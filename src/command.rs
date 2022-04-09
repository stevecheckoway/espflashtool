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

use std::cmp::min;
use std::time::Duration;

use binrw::binrw;

const DEFAULT_TIMEOUT: Duration = Duration::from_millis(3000);
const SYNC_TIMEOUT: Duration = Duration::from_millis(100);

#[derive(Debug, Clone)]
#[binrw]
#[brw(import(rom_loader: bool), little)]
pub enum Command {
    // Commands supported by the ESP8266 & ESP32 bootloaders.
    FlashBegin {
        total_size: u32,
        num_packets: u32,
        packet_size: u32,
        flash_offset: u32,
    },
    FlashData {
        data_size: u32,
        #[brw(pad_after = 8)]
        sequence_num: u32,
    },
    FlashEnd {
        reboot: u32,
    },
    MemBegin {
        total_size: u32,
        num_packets: u32,
        packet_size: u32,
        mem_offset: u32,
    },
    MemEnd {
        execute: u32,
        entry_point: u32,
    },
    MemData {
        data_size: u32,
        #[brw(pad_after = 8)]
        sequence_num: u32,
    },
    #[brw(magic = b"\x07\x07\x12 UUUUUUUUUUUUUUUUUUUUUUUUUUUUUUUU")]
    Sync,
    WriteReg {
        address: u32,
        value: u32,
        mask: u32,
        delay: u32,
    },
    ReadReg {
        address: u32,
    },

    // Commands supported by the ESP32 bootloader.
    SpiSetParams {
        id: u32,
        total_size: u32,
        block_size: u32,
        sector_size: u32,
        page_size: u32,
        status_mask: u32,
    },
    SpiAttach {
        pins: u32,
        #[br(if(rom_loader, 0))]
        #[bw(args_raw = rom_loader, write_with = |data: &u32, writer, opts, rom_loader| {
            if rom_loader {
                data.write_options(writer, opts, ())?;
            }
            Ok(())
        })]
        rom_only: u32,
    },
    ChangeBaudRate {
        new_rate: u32,
        old_rate: u32,
    },
    FlashDeflBegin {
        // Uncompressed size.
        total_size: u32,
        num_packets: u32,
        packet_size: u32,
        flash_offset: u32,
    },
    FlashDeflData {
        data_size: u32,
        #[brw(pad_after = 8)]
        sequence_num: u32,
    },
    FlashDeflEnd {
        reboot: u32,
    },
    SpiFlashMD5 {
        address: u32,
        #[brw(pad_after = 8)]
        size: u32,
    },

    // Stub-only commands.
    EraseFlash,
    EraseRegion {
        // XXX: Is this the offset from the start of flash?
        offset: u32,
        size: u32,
    },
    ReadFlash {
        offset: u32,
        read_length: u32,
        packet_size: u32,
        max_pending_packets: u32,
    },
    RunUserCode,
}

impl Command {
    pub fn name_from_code(code: u8) -> &'static str {
        match code {
            0x02 => "FlashBegin",
            0x03 => "FlashData",
            0x04 => "FlashEnd",
            0x05 => "MemBegin",
            0x06 => "MemEnd",
            0x07 => "MemData",
            0x08 => "Sync",
            0x09 => "WriteReg",
            0x0A => "ReadReg",
            0x0B => "SpiSetParams",
            0x0D => "SpiAttach",
            0x0F => "ChangeBaudRate",
            0x10 => "FlashDeflBegin",
            0x11 => "FlashDeflData",
            0x12 => "FlashDeflEnd",
            0x13 => "SpiFlashMD5",
            0xD0 => "EraseFlash",
            0xD1 => "EraseRegion",
            0xD2 => "ReadFlash",
            0xD3 => "RunUserCode",
            _ => "Unknown",
        }
    }

    pub fn spi_attach(hd_pin: u32, q_pin: u32, d_pin: u32, cs_pin: u32, clk_pin: u32) -> Self {
        let f = |pin: u32| match pin {
            0..=30 => pin,
            32 => 30,
            33 => 31,
            _ => panic!("Invalid pin assignment: {}", pin),
        };
        Command::SpiAttach {
            pins: (f(hd_pin) << 24)
                | (f(q_pin) << 18)
                | (f(d_pin) << 12)
                | (f(cs_pin) << 6)
                | f(clk_pin),
            rom_only: 0,
        }
    }

    pub fn code(&self) -> u8 {
        match self {
            Command::FlashBegin { .. } => 0x02,
            Command::FlashData { .. } => 0x03,
            Command::FlashEnd { .. } => 0x04,
            Command::MemBegin { .. } => 0x05,
            Command::MemEnd { .. } => 0x06,
            Command::MemData { .. } => 0x07,
            Command::Sync => 0x08,
            Command::WriteReg { .. } => 0x09,
            Command::ReadReg { .. } => 0x0A,
            Command::SpiSetParams { .. } => 0x0B,
            Command::SpiAttach { .. } => 0x0D,
            Command::ChangeBaudRate { .. } => 0x0F,
            Command::FlashDeflBegin { .. } => 0x10,
            Command::FlashDeflData { .. } => 0x11,
            Command::FlashDeflEnd { .. } => 0x12,
            Command::SpiFlashMD5 { .. } => 0x13,
            Command::EraseFlash => 0xD0,
            Command::EraseRegion { .. } => 0xD1,
            Command::ReadFlash { .. } => 0xD2,
            Command::RunUserCode => 0xD3,
        }
    }

    pub fn mem_begin(mem_offset: u32, total_size: u32) -> Self {
        let packet_size = min(total_size, 0x4000);
        Command::MemBegin {
            total_size,
            num_packets: (total_size + packet_size - 1) / packet_size,
            packet_size,
            mem_offset,
        }
    }

    pub fn flash_begin(flash_offset: u32, total_size: u32) -> Self {
        let packet_size = min(total_size, 0x4000);
        Command::FlashBegin {
            total_size,
            num_packets: (total_size + packet_size - 1) / packet_size,
            packet_size,
            flash_offset,
        }
    }

    pub fn timeout(&self) -> Duration {
        match self {
            Command::Sync => SYNC_TIMEOUT,
            _ => DEFAULT_TIMEOUT,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CommandError {
    #[error("Received message is invalid")]
    ReceivedMessageInvalid,

    #[error("Failed to act on received message")]
    FailedToActOnMessage,

    #[error("Invalid CRC in message")]
    InvalidCrc,

    #[error("Flash write error")]
    FlashWriteError,

    #[error("Flash read error")]
    FlashReadError,

    #[error("Flash read length error")]
    FlashReadLengthError,

    #[error("Deflate error")]
    DeflateError,

    #[error("Bad data length")]
    BadDataLen,

    #[error("Bad data checksum")]
    BadDataChecksum,

    #[error("Bad blocksize")]
    BadBlocksize,

    #[error("Invalid command")]
    InvalidCommand,

    #[error("Failed SPI operation")]
    FailedSpiOp,

    #[error("Failed SPI unlock")]
    FailedSpiUnlock,

    #[error("Not in flash mode")]
    NotInFlashMode,

    #[error("Inflate error")]
    InflateError,

    #[error("Not enough data")]
    NotEnoughData,

    #[error("Too much data")]
    TooMuchData,

    #[error("Command not implemented")]
    CommandNotImplemented,

    #[error("Unknown error code")]
    UnknownErrorCode,

    #[error("InvalidResponse")]
    InvalidResponse,
}

impl From<u8> for CommandError {
    fn from(value: u8) -> Self {
        use CommandError::*;
        match value {
            0x05 => ReceivedMessageInvalid,
            0x06 => FailedToActOnMessage,
            0x07 => InvalidCrc,
            0x08 => FlashWriteError,
            0x09 => FlashReadError,
            0x0A => FlashReadLengthError,
            0x0B => DeflateError,
            0xC0 => BadDataLen,
            0xC1 => BadDataChecksum,
            0xC2 => BadBlocksize,
            0xC3 => InvalidCommand,
            0xC4 => FailedSpiOp,
            0xC5 => FailedSpiUnlock,
            0xC6 => NotInFlashMode,
            0xC7 => InflateError,
            0xC8 => NotEnoughData,
            0xC9 => TooMuchData,
            0xFF => CommandNotImplemented,
            _ => UnknownErrorCode,
        }
    }
}

// The stub loader and the ESP8266 ROM loaders use the final two bytes of data
// for the status and error bytes. The various ESP32 ROM loaders use four
// bytes: status, error, 0, 0.
//
// Unfortunately, it's not possible to know which ROM loader (or even the stub
// loader) we're talking to for the first SYNC command. However, for all of
// the commands and chips this flasher knows about, the length of the data
// itself determines the size. Most responses have no data other than the
// status/error. Command::SpiFlashMd5 is the only command which has any data
// and that is only supported by the ESP32 and later.
#[inline]
fn status_size(data_len: u16) -> usize {
    match data_len {
        2 => 2,  // [stub, ESP8266]: status, error
        4 => 4,  // [ESP32]:         status, error, 0, 0
        18 => 2, // [stub]:          MD5 hash (bin), status, error
        36 => 4, // [ESP32]:         MD5 hash (hex), status, error, 0, 0
        _ => 2,  // This doesn't occur with the current commands and chips.
    }
}

#[binrw]
#[brw(little, magic = 0x01u8)]
pub struct ResponsePacket {
    pub cmd_code: u8,
    #[br(temp, assert(data_size >= 2))]
    #[bw(calc = data.len() as u16 + if reserved.is_none() { 2 } else { 4 })]
    data_size: u16,
    pub value: u32,
    #[br(count = (data_size as usize).saturating_sub(status_size(data_size)))]
    pub data: Vec<u8>,
    pub status: u8,
    pub error: u8,
    #[br(if(status_size(data_size) == 4))]
    pub reserved: Option<[u8; 2]>,
}
