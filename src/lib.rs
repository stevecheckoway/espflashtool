#![allow(dead_code)]

mod command;
pub mod event;
mod flasher;

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
