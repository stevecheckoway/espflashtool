#![allow(dead_code)]

mod chip;
mod command;
pub mod event;
mod flasher;
pub mod image;
pub mod partition;

pub use anyhow::{Error, Result};
pub use flasher::Flasher;

pub mod timeout {
    pub trait ErrorExt {
        fn is_timeout(&self) -> bool;
    }
}

impl timeout::ErrorExt for Error {
    fn is_timeout(&self) -> bool {
        self.downcast_ref::<std::io::Error>()
            .map_or(false, |err| err.kind() == std::io::ErrorKind::TimedOut)
    }
}

impl<T> timeout::ErrorExt for Result<T> {
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

