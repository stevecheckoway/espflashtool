use crate::{Result, Error};
use binrw::{binread, BinRead};

use crate::chip::Chip;
use crate::image::{EspImage, EspImageSegment};

const EM_XTENSA: u16 = 94;
const PT_LOAD: u32 = 1;

// Only supports 32-bit, little-endian ELF files.
#[binread]
#[br(little, magic = b"\x7FELF")]
struct ElfHeader {
    // ei_ident.
    #[br(temp, assert(ei_class == 1), err_context("Not 32-bit"))]
    ei_class: u8,
    #[br(temp, assert(ei_data == 1), err_context("Not little-endian"))]
    ei_data: u8,
    #[br(temp, assert(ei_version == 1), err_context("Unknown ELF file version"))]
    ei_version: u8,

    // Remainder of the fields.
    #[br(temp, pad_before = 9, assert(e_type == 2), err_context("Not an executable ELF file"))]
    e_type: u16,

    e_machine: u16,
    #[br(temp, assert(e_version == 1), err_context("Unknown ELF file version"))]
    e_version: u32,
    e_entry: u32,
    e_phoff: u32,
    e_shoff: u32,
    e_flags: u32,
    #[br(temp, assert(e_ehsize == 16 + 36), err_context("Invalid ELF header size"))]
    e_ehsize: u16,
    #[br(temp, assert(e_phentsize == 32), err_context("Invalid program header size"))]
    e_phentsize: u16,
    e_phnum: u16,
    #[br(temp, assert(e_shentsize == 0 || e_shentsize == 40))]
    e_shentsize: u16,
    e_shnum: u16,
    e_shstrndx: u16,
}

#[binread]
#[br(little)]
struct ElfProgramHeader {
    p_type: u32,
    p_offset: u32,
    p_vaddr: u32,
    p_paddr: u32,
    p_filesz: u32,
    p_memsz: u32,
    p_flags: u32,
    p_align: u32,
}

pub fn elf_to_image(chip: Chip, data: &[u8]) -> Result<EspImage> {
    let mut cursor = std::io::Cursor::new(data);
    let elf_header = ElfHeader::read(&mut cursor)?;
    let mut image: EspImage = Default::default();

    image.header.chip_id = chip.image_chip_id();
    image.header.entry_addr = elf_header.e_entry;

    let pheader_size = 32 * elf_header.e_phnum as usize;
    let pheader_offset = elf_header.e_phoff as usize;
    let pheader_end = pheader_size + pheader_offset;

    if pheader_end > data.len() {
        return Err(Error::FormatError("Invalid ELF program header table".into()));
    }
    let mut cursor = std::io::Cursor::new(&data[pheader_offset..pheader_end]);
    for _ in 0..elf_header.e_phnum {
        let pheader = ElfProgramHeader::read(&mut cursor)?;
        if pheader.p_type != PT_LOAD {
            continue;
        }
        let start = pheader.p_offset as usize;
        let size = pheader.p_filesz as usize;
        if start + size > data.len() {
            return Err(Error::FormatError("Invalid program header".into()));
        }
        let padded_size = (pheader.p_filesz as usize + 3) & !3;
        let mut seg_data: Vec<u8> = Vec::with_capacity(padded_size);
        seg_data.extend(&data[start..start+size]);
        seg_data.resize(padded_size, 0);
        image.segments.push(EspImageSegment {
            load_addr: pheader.p_vaddr,
            data: seg_data,
        });
    }
    image.update_metadata();
    println!("{image}");
    Ok(image)
}
