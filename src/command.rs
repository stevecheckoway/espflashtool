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
    ChangeBaudRate {
        new_rate: u32,
    },
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
            // 0x?? => "Command::"GetSecurityInfo",
            0xD0 => "EraseFlash",
            0xD1 => "EraseRegion",
            0xD2 => "ReadFlash",
            0xD3 => "RunUserCode",
            // 0x?? => "FlashEncryptData",
            _ => "Unknown",
        }
    }

    pub fn response_data_len_from_code(code: u8, rom_loader: bool) -> usize {
        match code {
            0x02..=0x0B | 0x0D | 0x0F..=0x12 | 0xD0..=0xD3 => 0,
            0x13 if rom_loader => 32,
            0x13 => 16,
            _ => usize::MAX,
        }
    }

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
            Command::ChangeBaudRate { .. } => 0x0F,
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

    #[error("Unknown error code")]
    UnknownErrorCode,

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
            _ => CommandError::UnknownErrorCode,
        }
    }
}
