use anyhow::{Result, Error, bail};

use crate::{from_le32, from_le16};

#[derive(Debug, Clone)]
pub struct EspPartitionTable {
    pub entries: Vec<PartitionEntry>,
    pub hash: [u8; 16],
}

impl TryFrom<&[u8]> for EspPartitionTable {
    type Error = Error;

    fn try_from(data: &[u8]) -> Result<Self> {
        let mut entries = Vec::new();
        for part in data.chunks_exact(32) {
            let magic = from_le16(&part[0..2]);
            match magic {
                0x50AA => entries.push(PartitionEntry::try_from(part)?),
                0xEBEB => {
                    return Ok(EspPartitionTable {
                        entries,
                        hash: (&part[16..]).try_into().unwrap()
                    });
                }
                _ => bail!("Unknown partition magic {:04X}", magic),
            }
        }
        bail!("No MD5 hash found");
    }
}

#[derive(Debug, Clone)]
pub struct PartitionEntry {
    pub magic: u16,
    pub r#type: u8,
    pub subtype: u8,
    pub offset: u32,
    pub size: u32,
    pub label: [u8; 16],
    pub flags: u32,
}

impl TryFrom<&[u8]> for PartitionEntry {
    type Error = Error;

    fn try_from(part: &[u8]) -> Result<Self> {
        if part.len() < 32 {
            bail!("Partition length {} too small", part.len());
        }
        Ok(PartitionEntry {
            magic: from_le16(&part[0..2]),
            r#type: part[2],
            subtype: part[3],
            offset: from_le32(&part[4..8]),
            size: from_le32(&part[8..12]),
            label: (&part[12..28]).try_into().unwrap(),
            flags: from_le32(&part[28..32]),
        })
    }
}

impl PartitionEntry {
    pub fn to_vec(&self) -> Vec<u8> {
        let mut r = Vec::new();
        r.extend(self.magic.to_le_bytes());
        r.push(self.r#type);
        r.push(self.subtype);
        r.extend(self.offset.to_le_bytes());
        r.extend(self.size.to_le_bytes());
        r.extend_from_slice(&self.label);
        r.extend(self.flags.to_le_bytes());
        r
    }
}
