use std::time::Duration;

const DEFAULT_TIMEOUT: Duration = Duration::from_millis(3000);
const SYNC_TIMEOUT: Duration = Duration::from_millis(10);

#[derive(Clone, Debug)]
pub enum Command {
    // Commands supported by the ESP8266 & ESP32 bootloaders.
    FlashBegin {
        total_size: u32,
        num_packets: u32,
        packet_size: u32,
        flash_offset: u32,
    },
    FlashData,
    FlashEnd,
    MemBegin,
    MemEnd,
    MemData,
    Sync,
    WriteReg,
    ReadReg {
        address: u32,
    },

    // Commands supported by the ESP32 bootloader.
    SpiSetParams,
    SpiAttach,
    ChangeBaudRate,
    FlashDeflBegin,
    FlashDeflData,
    FlashDeflEnd,
    SpiFlashMD5,

    // Commands supported by the ESP32S2 and later bootloaders.
    GetSecurityInfo,

    // Stub-only commands.
    EraseFlash,
    EraseRegion,
    ReadFlash,
    RunUserCode,

    // Flash encryption debug mode supported command.
    FlashEncryptData,
}

impl Command {
    pub fn code(&self) -> u8 {
        match self {
            Command::FlashBegin { .. } => 0x02,
            Command::FlashData => 0x03,
            Command::FlashEnd => 0x04,
            Command::MemBegin => 0x05,
            Command::MemEnd => 0x06,
            Command::MemData => 0x07,
            Command::Sync => 0x08,
            Command::WriteReg => 0x09,
            Command::ReadReg { .. } => 0x0A,
            Command::SpiSetParams => 0x0B,
            Command::SpiAttach => 0x0D,
            Command::ChangeBaudRate => 0x0F,
            Command::FlashDeflBegin => 0x10,
            Command::FlashDeflData => 0x11,
            Command::FlashDeflEnd => 0x12,
            Command::SpiFlashMD5 => 0x13,
            Command::GetSecurityInfo => todo!(),
            Command::EraseFlash => 0xD0,
            Command::EraseRegion => 0xD1,
            Command::ReadFlash => 0xD2,
            Command::RunUserCode => 0xD3,
            Command::FlashEncryptData => todo!(),
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

    #[error("InvalidResponse")]
    InvalidResponse,
}

impl From<u8> for CommandError {
    fn from(value: u8) -> Self {
        match value {
            0x05 => CommandError::ReceivedMessageInvalid,
            0x06 => CommandError::FailedToActOnMessage,
            0x07 => CommandError::InvalidCrc,
            0x08 => CommandError::FlashWriteError,
            0x09 => CommandError::FlashReadError,
            0x0A => CommandError::FlashReadLengthError,
            0x0B => CommandError::DeflateError,
            _ => CommandError::InvalidResponse,
        }
    }
}
