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

use binrw::binread;

use crate::Chip;

#[binread]
#[br(little, magic = b"STUB")]
pub struct Stub {
    pub chip: u32,
    pub entry: u32,
    pub text_start: u32,
    #[br(temp)]
    text_len: u32,
    #[br(count = text_len)]
    pub text: Vec<u8>,
    pub data_start: u32,
    #[br(temp)]
    data_len: u32,
    #[br(count = data_len)]
    pub data: Vec<u8>,
}

impl Stub {
    pub fn chip(&self) -> Option<Chip> {
        match self.chip {
            0..=0xFFFF => Chip::try_from_image_chip_id(self.chip as u16),
            0x10000 => Some(Chip::Esp8266),
            _ => None,
        }
    }
}
