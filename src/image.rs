use anyhow::bail;

use crate::{from_le32, from_le16};

#[derive(Debug, Clone, Copy)]
pub enum EspChipId {
    ESP32 = 0x0000,
    ESP32S2 = 0x0002,
    ESP32C3 = 0x0005,
    ESP32S3 = 0x0009,
    ESP32C2 = 0x000C,
}

#[derive(Debug, Clone)]
pub struct EspImageHeader {
    pub magic: u8,
    pub segment_count: u8,
    pub spi_mode: u8,
    pub spi_speed_size: u8,
    pub entry_addr: u32,
    pub wp_pin: u8,
    pub spi_pin_drv: [u8; 3],
    pub chip_id: u16,
    pub min_chip_rev: u8,
    pub reserved: [u8; 8],
    pub hash_appended: u8,
}

impl TryFrom<&[u8]> for EspImageHeader {
    type Error = anyhow::Error;

    fn try_from(hdr: &[u8]) -> anyhow::Result<Self> {
        if hdr.len() < 24 {
            bail!("Header length {} too small", hdr.len());
        }
        Ok(EspImageHeader {
            magic: hdr[0],
            segment_count: hdr[1],
            spi_mode: hdr[2],
            spi_speed_size: hdr[3],
            entry_addr: from_le32(&hdr[4..8]),
            wp_pin: hdr[8],
            spi_pin_drv: hdr[9..12].try_into().unwrap(),
            chip_id: from_le16(&hdr[12..14]),
            min_chip_rev: hdr[14],
            reserved: hdr[15..23].try_into().unwrap(),
            hash_appended: hdr[23],
        })
    }
}
