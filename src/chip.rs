use anyhow::bail;

use crate::{Error, Result};

pub struct SpiRegs {
    pub cmd: u32,
    pub addr: u32,
    pub user: u32,
    pub user1: u32,
    pub user2: u32,
    pub mosi_dlen: u32,
    pub miso_dlen: u32,
    pub w0: u32,
}

impl SpiRegs {
    #[inline]
    pub fn w(&self, index: usize) -> u32 {
        assert!(index < 16, "SPI data register {index} is out of range");
        self.w0 + (index as u32) * 4
    }
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

    pub fn image_chip_id(self) -> u16 {
        match self {
            Chip::Esp8266 | Chip::Esp32 => 0,
            Chip::Esp32S2 => 2,
            Chip::Esp32S3 => 9,
            Chip::Esp32C3 => 5,
        }
    }

    pub fn try_from_image_chip_id(id: u16) -> Option<Self> {
        match id {
            0 => Some(Chip::Esp32),
            2 => Some(Chip::Esp32S2),
            5 => Some(Chip::Esp32C3),
            9 => Some(Chip::Esp32S3),
            _ => None,
        }
    }

    pub fn spi_regs(self) -> SpiRegs {
        match self {
            Chip::Esp8266 => SpiRegs {
                cmd: 0x60000100,
                addr: 0x60000104,
                user: 0x6000011C,
                user1: 0x60000120,
                user2: 0x60000124,
                mosi_dlen: 0,
                miso_dlen: 0,
                w0: 0x60000140,
            },
            Chip::Esp32 => SpiRegs {
                cmd: 0x3FF42000,
                addr: 0x3FF42004,
                user: 0x3FF4201C,
                user1: 0x3FF42020,
                user2: 0x3FF42024,
                mosi_dlen: 0x3FF42028,
                miso_dlen: 0x3FF4202C,
                w0: 0x3FF42080,
            },
            Chip::Esp32S2 => SpiRegs {
                cmd: 0x3F402000,
                addr: 0x3F402004,
                user: 0x3F402018,
                user1: 0x3F40201C,
                user2: 0x3F402020,
                mosi_dlen: 0x3F402024,
                miso_dlen: 0x3F402028,
                w0: 0x3F402098,
            },
            Chip::Esp32S3 | Chip::Esp32C3 => SpiRegs {
                cmd: 0x60002000,
                addr: 0x60002004,
                user: 0x60002018,
                user1: 0x6000201C,
                user2: 0x60002020,
                mosi_dlen: 0x60002024,
                miso_dlen: 0x60002028,
                w0: 0x60002058,
            },
        }
    }
}

impl TryFrom<&str> for Chip {
    type Error = Error;

    fn try_from(value: &str) -> Result<Self> {
        Ok(match value {
            "esp8266" => Chip::Esp8266,
            "esp32" => Chip::Esp32,
            "esp32s2" => Chip::Esp32S2,
            "esp32s3" => Chip::Esp32S3,
            "esp32c3" => Chip::Esp32C3,
            _ => bail!("Unknown chip: {value}"),
        })
    }
}

impl std::fmt::Display for Chip {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Chip::Esp8266 => "ESP8266",
            Chip::Esp32 => "ESP32",
            Chip::Esp32S2 => "ESP32-S2",
            Chip::Esp32S3 => "ESP32-S3",
            Chip::Esp32C3 => "ESP32-C3",
        })
    }
}
