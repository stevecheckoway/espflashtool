#![allow(dead_code)]
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

mod chip;
mod command;
mod elf;
pub mod event;
mod flasher;
pub mod image;
pub mod partition;
mod stub;

pub use chip::Chip;
use command::CommandError;
pub use elf::elf_to_image;
pub use flasher::Flasher;
use flasher::FlasherError;

#[derive(thiserror::Error, Debug)]
pub enum Error {
    /// Represents a command error.
    #[error(transparent)]
    CommandError(#[from] CommandError),

    /// Represents a flasher error.
    #[error(transparent)]
    FlasherError(#[from] FlasherError),

    /// Represents a serial port error.
    #[error(transparent)]
    SerialPortError(#[from] serialport::Error),

    /// Represents a binary format error.
    #[error("Format error: {}", .0)]
    FormatError(String),

    /// Represents an I/O error.
    #[error(transparent)]
    IOError(#[from] std::io::Error),
}

impl From<binrw::Error> for Error {
    fn from(err: binrw::Error) -> Self {
        Error::FormatError(err.to_string())
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

pub mod timeout {
    pub trait ErrorExt {
        fn is_timeout(&self) -> bool;
    }
}

impl timeout::ErrorExt for Error {
    fn is_timeout(&self) -> bool {
        match self {
            Error::IOError(err) => err.kind() == std::io::ErrorKind::TimedOut,
            Error::SerialPortError(err) => {
                err.kind() == serialport::ErrorKind::Io(std::io::ErrorKind::TimedOut)
            }
            _ => false,
        }
    }
}

impl<T> timeout::ErrorExt for Result<T, Error> {
    fn is_timeout(&self) -> bool {
        self.as_ref().err().map_or(false, |err| err.is_timeout())
    }
}

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
