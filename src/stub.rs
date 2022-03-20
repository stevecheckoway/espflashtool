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
            _ => None
        }
    }
}
