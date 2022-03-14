use std::io::{Seek, SeekFrom, Write};

use crate::{Error, Result};
use binrw::{binrw, BinRead, BinReaderExt, BinWrite, ReadOptions, WriteOptions};
use sha2::{Digest, Sha256};

use crate::chip::Chip;
#[derive(Debug, Clone, Copy)]
#[repr(u16)]
pub enum EspChipId {
    ESP32 = 0x0000,
    ESP32S2 = 0x0002,
    ESP32C3 = 0x0005,
    ESP32S3 = 0x0009,
    ESP32C2 = 0x000C,
}

#[derive(Default, Debug, Clone)]
#[binrw]
#[brw(little, magic = b"\xe9")]
pub struct EspImageHeader {
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

#[binrw]
#[brw(little)]
pub struct EspImageSegment {
    pub load_addr: u32,
    #[br(temp)]
    #[bw(calc = data.len() as u32)]
    data_len: u32,

    #[br(count = data_len)]
    pub data: Vec<u8>,
}

impl std::fmt::Debug for EspImageSegment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EspImageSegment")
            .field("load_addr", &self.load_addr)
            .field("data_len", &self.data.len())
            .finish()
    }
}

impl Clone for EspImageSegment {
    fn clone(&self) -> Self {
        Self {
            load_addr: self.load_addr,
            data: self.data.clone(),
        }
    }
}

fn checksum_parser<R: std::io::Read + std::io::Seek>(
    reader: &mut R,
    _ro: &ReadOptions,
    _: (),
) -> binrw::BinResult<u8> {
    let delta = 15 - reader.stream_position()? % 16;
    if delta > 0 {
        reader.seek(SeekFrom::Current(delta as i64))?;
    }
    reader.read_le()
}

fn checksum_writer<W: binrw::io::Write + binrw::io::Seek>(
    &checksum: &u8,
    writer: &mut W,
    _opts: &WriteOptions,
    _: (),
) -> binrw::BinResult<()> {
    const ZEROS: [u8; 15] = [0; 15];
    let pos = (writer.stream_position()? % 16) as usize;
    if pos < 15 {
        writer.write(&ZEROS[pos..])?;
    }
    writer.write(&[checksum])?;
    Ok(())
}

#[derive(Default, Debug, Clone)]
#[binrw]
#[brw(little)]
pub struct EspImage {
    pub header: EspImageHeader,
    #[br(count = header.segment_count)]
    pub segments: Vec<EspImageSegment>,
    #[br(parse_with = checksum_parser)]
    #[bw(write_with = checksum_writer)]
    pub checksum: u8,
    #[br(if(header.hash_appended != 0))]
    pub hash: Option<[u8; 32]>,
}

impl TryFrom<&[u8]> for EspImage {
    type Error = Error;

    fn try_from(data: &[u8]) -> Result<Self> {
        let mut cursor = std::io::Cursor::new(data);
        Ok(Self::read(&mut cursor)?)
    }
}

impl std::fmt::Display for EspImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_fmt(format_args!("Entry point: 0x{:08X}\n", self.header.entry_addr))?;
        if let Some(chip_id) = Chip::try_from_image_chip_id(self.header.chip_id) {
            f.write_fmt(format_args!("Chip Id: {chip_id}\n"))?;
        } else {
            f.write_fmt(format_args!("Chip Id: 0x{:04X}\n", self.header.chip_id))?;
        }


        f.write_fmt(format_args!("{} segment{}\n\n",
            self.header.segment_count,
            if self.header.segment_count == 1 { "" } else { "s" },
        ))?;
        let mut offset: usize = 24 + 8;
        for (num, seg) in self.segments.iter().enumerate() {
            let num = num + 1;
            let len = seg.data.len();
            let addr = seg.load_addr;
            f.write_fmt(format_args!("Segment {num}: len 0x{len:05X} load 0x{addr:08X} file_offs 0x{offset:08X}\n"))?;
            offset += 8 + len;
        }
        let expected_sum = self.compute_checksum();
        let actual_sum = self.checksum;
        let valid = if actual_sum == expected_sum { "valid" } else { "invalid" };
        f.write_fmt(format_args!("\nChecksum: {actual_sum:02X} ({valid})"))?;

        if let Some(ref actual_hash) = self.hash {
            let expected_hash = self.compute_hash();
            let valid = if actual_hash == &expected_hash { "valid" } else { "invalid" };
            f.write_str("\nHash: ")?;
            for &x in actual_hash {
                f.write_fmt(format_args!("{:02X}", x))?;
            }
            f.write_fmt(format_args!(" ({valid})"))?;
        }
        
        Ok(())
    }
}

struct HashWrapper {
    pos: u64,
    hasher: sha2::Sha256,
}

impl Write for HashWrapper {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.hasher.update(buf);
        self.pos += buf.len() as u64;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
}

impl Seek for HashWrapper {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        if matches!(pos, SeekFrom::Current(0))
            || matches!(pos, SeekFrom::Start(pos) if pos == self.pos)
        {
            Ok(self.pos)
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "Not supported",
            ))
        }
    }
}

impl EspImage {
    pub fn compute_checksum(&self) -> u8 {
        let mut sum = 0xEFu8;
        for seg in &self.segments {
            for &x in &seg.data {
                sum ^= x;
            }
        }
        sum
    }

    pub fn compute_hash(&self) -> [u8; 32] {
        let mut hasher = HashWrapper {
            pos: 0,
            hasher: Sha256::new(),
        };
        self.header.write_to(&mut hasher).unwrap();
        for seg in &self.segments {
            seg.write_to(&mut hasher).unwrap();
        }
        checksum_writer(&self.checksum, &mut hasher, &Default::default(), ()).unwrap();
        hasher.hasher.finalize().into()
    }

    /**
     * Update the image's segment count, checksum, and hash.
     */
    pub fn update_metadata(&mut self) {
        self.header.segment_count = self.segments.len().try_into().unwrap();
        self.header.hash_appended = 1;
        self.checksum = self.compute_checksum();
        self.hash = Some(self.compute_hash());
    }
}

#[cfg(test)]
mod test {
    use std::io::Cursor;
    use anyhow::Result;
    use binrw::BinWrite;

    use super::*;

    fn bin<BW>(bw: BW) -> Result<Vec<u8>> where
        BW: BinWrite,
        <BW as BinWrite>::Args: Default
    {
        let mut data: Vec<u8> = Vec::new();
        bw.write_to(&mut Cursor::new(&mut data))?;
        Ok(data)
    }

    #[test]
    fn test_write_segment() -> Result<()> {
        let data = bin(EspImageSegment {
            load_addr: 0xAABBCCDD,
            data: vec![0, 1, 2, 3, 4],
        })?;
        assert_eq!(&data, b"\xDD\xCC\xBB\xAA\x05\x00\x00\x00\x00\x01\x02\x03\x04");
        Ok(())
    }

    #[test]
    fn test_header() -> Result<()> {
        let data = bin(EspImageHeader {
            segment_count: 0x6,
            spi_mode: 0x0,
            spi_speed_size: 0x0,
            entry_addr: 0x40081cf0,
            wp_pin: 0xee,
            spi_pin_drv: [0; 3],
            chip_id: 0x0,
            min_chip_rev: 0x0,
            reserved: [0; 8],
            hash_appended: 0x1,
        })?;
        assert_eq!(&data, b"\xe9\x06\x00\x00\xF0\x1C\x08\x40\xEE\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x01");
        Ok(())
    }
}
