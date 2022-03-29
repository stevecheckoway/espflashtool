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

use crate::{Error, Result};
use binrw::{BinRead, BinWrite};
use std::borrow::Cow;
use std::fmt::{Display, Write};
use std::io::Cursor;

pub const TYPE_APP: u8 = 0;
pub const SUBTYPE_APP_FACTORY: u8 = 0;
pub const SUBTYPE_APP_TEST: u8 = 0x20;
pub const TYPE_DATA: u8 = 1;
pub const SUBTYPE_DATA_OTA: u8 = 0;
pub const SUBTYPE_DATA_PHY: u8 = 1;
pub const SUBTYPE_DATA_NVS: u8 = 2;
pub const SUBTYPE_DATA_COREDUMP: u8 = 3;
pub const SUBTYPE_DATA_NVS_KEYS: u8 = 4;
pub const SUBTYPE_DATA_EFUSE_EM: u8 = 5;
pub const SUBTYPE_DATA_ESPHTTPD: u8 = 0x80;
pub const SUBTYPE_DATA_FAT: u8 = 0x81;
pub const SUBTYPE_DATA_SPIFFS: u8 = 0x82;

#[derive(BinRead, BinWrite, Debug, Clone)]
pub struct EspPartitionTable {
    #[br(parse_with(binrw::until(|pe: &PartitionEntry| matches!(pe, PartitionEntry::Hash { .. }))))]
    pub entries: Vec<PartitionEntry>,
}

impl TryFrom<&[u8]> for EspPartitionTable {
    type Error = Error;

    fn try_from(data: &[u8]) -> Result<Self> {
        let mut cursor = Cursor::new(data);
        Ok(Self::read(&mut cursor)?)
    }
}

impl Display for EspPartitionTable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for pe in &self.entries {
            f.write_fmt(format_args!("{pe}\n"))?;
        }
        Ok(())
    }
}

#[derive(BinRead, BinWrite, Debug, Clone)]
#[brw(little)]
pub enum PartitionEntry {
    #[brw(magic = b"\xAA\x50")]
    Partition {
        type_: u8,
        subtype: u8,
        offset: u32,
        size: u32,
        label: [u8; 16],
        flags: u32,
    },
    #[brw(magic = b"\xEB\xEB")]
    Hash {
        #[br(pad_before = 14)]
        digest: [u8; 16],
    },
}

fn type_name(type_: u8, subtype: u8) -> (Cow<'static, str>, Cow<'static, str>) {
    fn hex(x: u8) -> Cow<'static, str> {
        Cow::Owned(format!("0x{x:02X}"))
    }

    match type_ {
        TYPE_APP => (
            Cow::Borrowed("app"),
            match subtype {
                SUBTYPE_APP_FACTORY => Cow::Borrowed("factory"),
                0x10..=0x1F => Cow::Owned(format!("ota{subtype:02X}")),
                SUBTYPE_APP_TEST => Cow::Borrowed("test"),
                _ => hex(subtype),
            },
        ),
        TYPE_DATA => (
            Cow::Borrowed("data"),
            match subtype {
                SUBTYPE_DATA_OTA => Cow::Borrowed("ota"),
                SUBTYPE_DATA_PHY => Cow::Borrowed("phy"),
                SUBTYPE_DATA_NVS => Cow::Borrowed("nvs"),
                SUBTYPE_DATA_COREDUMP => Cow::Borrowed("coredump"),
                SUBTYPE_DATA_NVS_KEYS => Cow::Borrowed("nvs_keys"),
                SUBTYPE_DATA_EFUSE_EM => Cow::Borrowed("efuse"),
                SUBTYPE_DATA_ESPHTTPD => Cow::Borrowed("esphttpd"),
                SUBTYPE_DATA_FAT => Cow::Borrowed("fat"),
                SUBTYPE_DATA_SPIFFS => Cow::Borrowed("spiffs"),
                _ => hex(subtype),
            },
        ),
        _ => (hex(type_), hex(subtype)),
    }
}

impl Display for PartitionEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PartitionEntry::Partition {
                type_,
                subtype,
                offset,
                size,
                label,
                flags,
            } => {
                let label = if let Some(last) = label.iter().rposition(|x| *x != 0) {
                    &label[..=last]
                } else {
                    label
                };
                for ch in label.iter().flat_map(|ch| std::ascii::escape_default(*ch)) {
                    f.write_char(ch.into())?;
                }
                let (t_name, s_name) = type_name(*type_, *subtype);
                f.write_fmt(format_args!(": type={t_name} subtype={s_name} offset=0x{offset:X} size=0x{size:X} flags=0x{flags:x}"))?;
            }

            PartitionEntry::Hash { digest } => {
                f.write_str("Hash: ")?;
                for &b in digest {
                    f.write_fmt(format_args!("{b:02X}"))?
                }
            }
        }
        Ok(())
    }
}
