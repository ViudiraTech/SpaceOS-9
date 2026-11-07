/*
 *
 *       src/module/elf.rs
 *       Strict ET_REL module parser, section layout and architecture relocations
 *
 *       2026/11/6 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Loadable module linker.
//!
//! The input is a Linux-style `ET_REL` object built for the running
//! architecture.  Every allocatable section is page-aligned and mapped with
//! its final W^X protection; relocation writes go through the HHDM alias, so
//! text pages never need to be writable after they are published.  Symbols
//! resolve exclusively through the exported-symbol whitelist.

use alloc::{
    string::{String, ToString},
    vec::Vec,
};
use core::ptr;

use super::virt;
use crate::{
    arch::paging::KernelProt,
    mm::{pmm, vmm},
    module::api,
    vfs::abi,
};

pub type InitFn = unsafe extern "C" fn() -> i32;
pub type ExitFn = unsafe extern "C" fn();

pub const SPK_MODULE_MAGIC: u32 = 0x4d4b_5053; // "SPKM"
pub const SPK_MODULE_ABI_VERSION: u32 = 2;
const METADATA_SIZE: usize = 56;

const SHT_PROGBITS: u32 = 1;
const SHT_SYMTAB: u32 = 2;
const SHT_STRTAB: u32 = 3;
const SHT_RELA: u32 = 4;
const SHT_NOBITS: u32 = 8;
const SHT_INIT_ARRAY: u32 = 14;
const SHT_FINI_ARRAY: u32 = 15;
const SHT_PREINIT_ARRAY: u32 = 16;
const SHT_GROUP: u32 = 17;
const SHF_WRITE: u64 = 1;
const SHF_ALLOC: u64 = 2;
const SHF_EXECINSTR: u64 = 4;
const SHN_UNDEF: u16 = 0;
const SHN_LORESERVE: u16 = 0xff00;
const SHN_ABS: u16 = 0xfff1;
const SHN_COMMON: u16 = 0xfff2;
const STB_WEAK: u8 = 2;
const STT_OBJECT: u8 = 1;
const STT_FUNC: u8 = 2;
const STT_SECTION: u8 = 3;
const STT_FILE: u8 = 4;
const STT_TLS: u8 = 6;

#[cfg(target_arch = "x86_64")]
const EXPECTED_MACHINE: u16 = 62;
#[cfg(target_arch = "aarch64")]
const EXPECTED_MACHINE: u16 = 183;
#[cfg(target_arch = "riscv64")]
const EXPECTED_MACHINE: u16 = 243;

const MAX_MODULE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_SECTIONS: usize = 256;
const MAX_SYMBOLS: usize = 65536;
const MAX_RELOCATIONS: usize = 262144;
const MAX_NAME: usize = 255;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Class {
    Text,
    RoData,
    Data,
    Bss,
}

impl Class {
    fn protection(self) -> KernelProt {
        match self {
            Self::Text => KernelProt::ReadOnlyExec,
            Self::RoData => KernelProt::ReadOnly,
            Self::Data | Self::Bss => KernelProt::ReadWrite,
        }
    }
}

pub struct Section {
    pub name: String,
    pub va: u64,
    pub phys: u64,
    pub pages: usize,
    pub size: usize,
    pub class: Class,
}

pub struct ModuleSymbol {
    pub name: String,
    pub address: u64,
    pub kind: u8,
}

pub struct PendingModule {
    pub name: String,
    pub version: String,
    pub license: String,
    pub vermagic: String,
    pub init: InitFn,
    pub exit: Option<ExitFn>,
    pub sections: Vec<Section>,
    pub symbols: Vec<ModuleSymbol>,
    pub total_size: usize,
    pub va: u64,
    pub pages: usize,
}

impl PendingModule {
    /// Unpublish every page and return the frames to the PMM.
    pub fn release(&mut self) {
        release_sections(&self.sections);
        virt::deallocate(self.va, self.pages);
        self.sections.clear();
    }
}

/// Release a fully loaded module: unmap/free every section and return the
/// virtual range to the module allocator.
pub fn release_module(sections: &[Section], va: u64, pages: usize) {
    release_sections(sections);
    crate::module::virt::deallocate(va, pages);
}

pub fn release_sections(sections: &[Section]) {
    for section in sections {
        if section.pages == 0 {
            continue;
        }
        if vmm::unmap_kernel(section.va, section.pages).is_ok() {
            pmm::free(section.phys, section.pages).expect("module frame release failed");
        } else {
            // Leaving the frames allocated is safer than freeing memory that
            // may still be reachable through a stale translation.
            crate::kwarn!("module: could not unmap {:#x}", section.va);
        }
    }
}

fn errno(code: i64) -> i32 {
    -(code as i32)
}

fn align_up(value: u64, alignment: u64) -> Option<u64> {
    value
        .checked_add(alignment - 1)
        .map(|v| v & !(alignment - 1))
}

fn read_u16(data: &[u8], offset: usize) -> Result<u16, i32> {
    data.get(offset..offset + 2)
        .and_then(|bytes| bytes.try_into().ok())
        .map(u16::from_le_bytes)
        .ok_or(errno(abi::ENOEXEC))
}

fn read_u32(data: &[u8], offset: usize) -> Result<u32, i32> {
    data.get(offset..offset + 4)
        .and_then(|bytes| bytes.try_into().ok())
        .map(u32::from_le_bytes)
        .ok_or(errno(abi::ENOEXEC))
}

fn read_u64(data: &[u8], offset: usize) -> Result<u64, i32> {
    data.get(offset..offset + 8)
        .and_then(|bytes| bytes.try_into().ok())
        .map(u64::from_le_bytes)
        .ok_or(errno(abi::ENOEXEC))
}

#[derive(Clone, Copy)]
struct Shdr {
    name: u32,
    kind: u32,
    flags: u64,
    offset: u64,
    size: u64,
    link: u32,
    info: u32,
    addralign: u64,
    entsize: u64,
}

#[derive(Clone, Copy)]
struct Sym {
    name: u32,
    info: u8,
    shndx: u16,
    value: u64,
}

struct Elf<'a> {
    data: &'a [u8],
    shoff: u64,
    shentsize: usize,
    shnum: usize,
    shstrndx: usize,
}

impl<'a> Elf<'a> {
    fn parse(data: &'a [u8]) -> Result<Self, i32> {
        if data.len() < 64 || &data[0..4] != b"\x7fELF" {
            return Err(errno(abi::ENOEXEC));
        }
        if data[4] != 2 || data[5] != 1 || data[6] != 1 {
            return Err(errno(abi::ENOEXEC));
        }
        if read_u16(data, 16)? != 1 {
            return Err(errno(abi::ENOEXEC));
        }
        if read_u16(data, 18)? != EXPECTED_MACHINE {
            crate::kwarn!(
                "module: architecture mismatch (ELF machine {})",
                read_u16(data, 18)?
            );
            return Err(errno(abi::ENOEXEC));
        }
        let ehsize = read_u16(data, 52)? as usize;
        let shentsize = read_u16(data, 58)? as usize;
        let shnum = read_u16(data, 60)? as usize;
        let shstrndx = read_u16(data, 62)? as usize;
        let shoff = read_u64(data, 40)?;
        if ehsize != 64
            || shentsize != 64
            || shnum == 0
            || shnum > MAX_SECTIONS
            || shstrndx == 0
            || shstrndx >= shnum
        {
            return Err(errno(abi::ENOEXEC));
        }
        let table_end = shoff
            .checked_add((shnum as u64).checked_mul(64).ok_or(errno(abi::ENOEXEC))?)
            .ok_or(errno(abi::ENOEXEC))?;
        if table_end > data.len() as u64 {
            return Err(errno(abi::ENOEXEC));
        }
        Ok(Self {
            data,
            shoff,
            shentsize,
            shnum,
            shstrndx,
        })
    }

    fn shdr(&self, index: usize) -> Result<Shdr, i32> {
        if index >= self.shnum {
            return Err(errno(abi::ENOEXEC));
        }
        let offset = self.shoff as usize + index * self.shentsize;
        Ok(Shdr {
            name: read_u32(self.data, offset)?,
            kind: read_u32(self.data, offset + 4)?,
            flags: read_u64(self.data, offset + 8)?,
            offset: read_u64(self.data, offset + 24)?,
            size: read_u64(self.data, offset + 32)?,
            link: read_u32(self.data, offset + 40)?,
            info: read_u32(self.data, offset + 44)?,
            addralign: read_u64(self.data, offset + 48)?,
            entsize: read_u64(self.data, offset + 56)?,
        })
    }

    fn bytes(&self, offset: u64, size: u64) -> Result<&'a [u8], i32> {
        let end = offset.checked_add(size).ok_or(errno(abi::ENOEXEC))?;
        if end > self.data.len() as u64 {
            return Err(errno(abi::ENOEXEC));
        }
        Ok(&self.data[offset as usize..end as usize])
    }

    fn string(&self, table: &Shdr, offset: u32) -> Result<&'a str, i32> {
        if offset as u64 >= table.size {
            return Err(errno(abi::ENOEXEC));
        }
        let bytes = self.bytes(table.offset + offset as u64, table.size - offset as u64)?;
        let length = bytes
            .iter()
            .position(|&byte| byte == 0)
            .ok_or(errno(abi::ENOEXEC))?;
        core::str::from_utf8(&bytes[..length]).map_err(|_| errno(abi::ENOEXEC))
    }

    fn section_name(&self, shdr: &Shdr) -> Result<&'a str, i32> {
        let table = self.shdr(self.shstrndx)?;
        if table.kind != SHT_STRTAB {
            return Err(errno(abi::ENOEXEC));
        }
        self.string(&table, shdr.name)
    }

    fn symbol(&self, symtab: &Shdr, index: usize) -> Result<Sym, i32> {
        if symtab.entsize != 24 {
            return Err(errno(abi::ENOEXEC));
        }
        let count = symtab.size / 24;
        if index as u64 >= count {
            return Err(errno(abi::ENOEXEC));
        }
        let offset = symtab.offset as usize + index * 24;
        Ok(Sym {
            name: read_u32(self.data, offset)?,
            info: *self.data.get(offset + 4).ok_or(errno(abi::ENOEXEC))?,
            shndx: read_u16(self.data, offset + 6)?,
            value: read_u64(self.data, offset + 8)?,
        })
    }
}

fn classify(name: &str, shdr: &Shdr) -> Result<Option<Class>, i32> {
    if shdr.flags & SHF_ALLOC == 0 {
        return Ok(None);
    }
    match shdr.kind {
        SHT_PROGBITS => {
            if shdr.flags & SHF_EXECINSTR != 0 {
                Ok(Some(Class::Text))
            } else if name == ".spk_module" || name.starts_with(".data.rel.ro") {
                Ok(Some(Class::RoData))
            } else if shdr.flags & SHF_WRITE != 0 {
                Ok(Some(Class::Data))
            } else {
                Ok(Some(Class::RoData))
            }
        }
        SHT_NOBITS => Ok(Some(Class::Bss)),
        SHT_INIT_ARRAY | SHT_FINI_ARRAY | SHT_PREINIT_ARRAY | SHT_GROUP => {
            crate::kwarn!(
                "module: unsupported allocatable section {} ({})",
                name,
                shdr.kind
            );
            Err(errno(abi::ENOEXEC))
        }
        _ => {
            crate::kwarn!("module: unsupported allocatable section type {}", shdr.kind);
            Err(errno(abi::ENOEXEC))
        }
    }
}

struct Input {
    name: String,
    header: Shdr,
    class: Class,
    offset: u64,
    pages: usize,
    synthetic: bool,
}

fn phys_ptr(physical: u64) -> *mut u8 {
    pmm::phys_to_virt(physical) as *mut u8
}

fn module_string(sections: &[Section], address: u64, limit: usize) -> Result<String, i32> {
    let section = sections
        .iter()
        .find(|section| address >= section.va && address < section.va + section.size as u64)
        .ok_or(errno(abi::EFAULT))?;
    let offset = (address - section.va) as usize;
    let available = (section.size - offset).min(limit);
    // SAFETY: the address lies inside a section owned by this module and the
    // physical window has been mapped read-only for the module lifetime.
    let bytes =
        unsafe { core::slice::from_raw_parts(phys_ptr(section.phys + offset as u64), available) };
    let length = bytes
        .iter()
        .position(|&byte| byte == 0)
        .ok_or(errno(abi::ENOEXEC))?;
    core::str::from_utf8(&bytes[..length])
        .map(ToString::to_string)
        .map_err(|_| errno(abi::ENOEXEC))
}

fn read_phys_u32(physical: u64, offset: usize) -> u32 {
    // SAFETY: module sections are page-backed and the offset is bounds-checked
    // against the section size by the caller.
    unsafe { ptr::read_unaligned(phys_ptr(physical + offset as u64) as *const u32) }
}

fn read_phys_u64(physical: u64, offset: usize) -> u64 {
    // SAFETY: same invariant as read_phys_u32.
    unsafe { ptr::read_unaligned(phys_ptr(physical + offset as u64) as *const u64) }
}

fn resolve_symbol(
    elf: &Elf<'_>,
    symtab: &Shdr,
    strtab: &Shdr,
    loaded: &[Option<usize>],
    sections: &[Section],
    cache: &mut [Option<u64>],
    index: usize,
) -> Result<u64, i32> {
    if let Some(value) = cache.get(index).copied().flatten() {
        return Ok(value);
    }
    let symbol = elf.symbol(symtab, index)?;
    let binding = symbol.info >> 4;
    let value = if symbol.shndx == SHN_UNDEF {
        let name = elf.string(strtab, symbol.name)?;
        if name.is_empty() {
            return Err(errno(abi::ENOEXEC));
        }
        match api::lookup(name) {
            Some(address) => address,
            None if binding == STB_WEAK => 0,
            None => {
                crate::kwarn!("module: unresolved symbol '{}'", name);
                return Err(errno(abi::ENOENT));
            }
        }
    } else if symbol.shndx == SHN_ABS {
        symbol.value
    } else if symbol.shndx == SHN_COMMON {
        crate::kwarn!("module: common symbols are not supported");
        return Err(errno(abi::ENOEXEC));
    } else if symbol.shndx < SHN_LORESERVE {
        let Some(Some(section_index)) = loaded.get(symbol.shndx as usize) else {
            crate::kwarn!("module: symbol references a discarded section");
            return Err(errno(abi::ENOEXEC));
        };
        sections[*section_index]
            .va
            .checked_add(symbol.value)
            .ok_or(errno(abi::ENOEXEC))?
    } else {
        return Err(errno(abi::ENOEXEC));
    };
    if let Some(slot) = cache.get_mut(index) {
        *slot = Some(value);
    }
    Ok(value)
}

fn ensure_bounds(offset: u64, width: u64, size: u64) -> Result<(), i32> {
    if offset.checked_add(width).is_none_or(|end| end > size) {
        return Err(errno(abi::ENOEXEC));
    }
    Ok(())
}

fn write_u16(loc: *mut u8, value: u16) {
    // SAFETY: the caller validated the relocation width against the section.
    unsafe { ptr::write_unaligned(loc as *mut u16, value) }
}

fn write_u32(loc: *mut u8, value: u32) {
    // SAFETY: the caller validated the relocation width against the section.
    unsafe { ptr::write_unaligned(loc as *mut u32, value) }
}

fn write_u64(loc: *mut u8, value: u64) {
    // SAFETY: the caller validated the relocation width against the section.
    unsafe { ptr::write_unaligned(loc as *mut u64, value) }
}

fn read_u32_at(loc: *mut u8) -> u32 {
    // SAFETY: the caller validated the instruction width against the section.
    unsafe { ptr::read_unaligned(loc as *const u32) }
}

fn write_u32_at(loc: *mut u8, value: u32) {
    // SAFETY: the caller validated the instruction width against the section.
    unsafe { ptr::write_unaligned(loc as *mut u32, value) }
}

fn store_signed(loc: *mut u8, value: i128, bits: u32) -> Result<(), i32> {
    let minimum = -(1i128 << (bits - 1));
    let maximum = (1i128 << (bits - 1)) - 1;
    if value < minimum || value > maximum {
        return Err(errno(abi::ENOEXEC));
    }
    match bits {
        16 => write_u16(loc, value as u16),
        32 => write_u32(loc, value as u32),
        64 => write_u64(loc, value as u64),
        _ => return Err(errno(abi::ENOEXEC)),
    }
    Ok(())
}

fn store_unsigned(loc: *mut u8, value: i128, bits: u32) -> Result<(), i32> {
    if value < 0 || value > (1i128 << bits) - 1 {
        return Err(errno(abi::ENOEXEC));
    }
    match bits {
        16 => write_u16(loc, value as u16),
        32 => write_u32(loc, value as u32),
        64 => write_u64(loc, value as u64),
        _ => return Err(errno(abi::ENOEXEC)),
    }
    Ok(())
}

struct GotState {
    va: u64,
    phys: u64,
    size: usize,
    next: core::cell::Cell<usize>,
}

struct RelocContext<'a> {
    elf: &'a Elf<'a>,
    rela: Shdr,
    section_va: u64,
    section_size: u64,
    symbols: &'a Shdr,
    strtab: &'a Shdr,
    loaded: &'a [Option<usize>],
    sections: &'a [Section],
    got: Option<&'a GotState>,
}

fn relocate_x86_64(
    context: &RelocContext<'_>,
    r_type: u32,
    symbol: u64,
    addend: i64,
    place: u64,
    loc: *mut u8,
) -> Result<(), i32> {
    let symbol = symbol as i128;
    let addend = addend as i128;
    let place = place as i128;
    let relative = symbol + addend - place;
    match r_type {
        0 => Ok(()),
        1 => {
            write_u64(loc, (symbol + addend) as u64);
            Ok(())
        }
        2 | 4 => store_signed(loc, relative, 32),
        9 | 41 | 42 => {
            // GOTPCREL: dispatch through a module-owned GOT slot.
            let Some(got) = context.got else {
                return Err(errno(abi::ENOEXEC));
            };
            let index = got.next.get();
            if index * 8 + 8 > got.size {
                return Err(errno(abi::ENOEXEC));
            }
            let slot = got.va + (index as u64) * 8;
            // SAFETY: the synthetic GOT section owns at least slot+8 bytes.
            unsafe {
                ptr::write_unaligned(
                    phys_ptr(got.phys + (index as u64) * 8) as *mut u64,
                    symbol as u64,
                );
            }
            got.next.set(index + 1);
            let displacement = slot as i128 + addend - place;
            store_signed(loc, displacement, 32)
        }
        10 => store_unsigned(loc, symbol + addend, 32),
        11 => {
            // R_X86_64_32S stores the low 32 bits only if sign-extending them
            // reproduces the full 64-bit value. Kernel addresses live in the
            // top 2 GiB, so a plain i32 range check would reject every symbol.
            let value = (symbol + addend) as u64;
            let low = value as u32;
            if (low as i32) as i64 as u64 != value {
                crate::kwarn!(
                    "module: R_X86_64_32S value {:#x} does not sign-extend from {:#x}",
                    value,
                    low
                );
                return Err(errno(abi::ENOEXEC));
            }
            write_u32(loc, low);
            Ok(())
        }
        24 => {
            write_u64(loc, relative as u64);
            Ok(())
        }
        _ => {
            crate::kwarn!("module: unsupported x86_64 relocation {}", r_type);
            Err(errno(abi::ENOEXEC))
        }
    }
}

#[cfg(target_arch = "aarch64")]
fn adr_imm(insn: u32, immediate: i64) -> Result<u32, i32> {
    if immediate < -(1i64 << 20) || immediate >= (1i64 << 20) {
        return Err(errno(abi::ENOEXEC));
    }
    let immediate = immediate as u32 & 0x1f_ffff;
    Ok((insn & !((3 << 29) | (0x7ffff << 5)))
        | ((immediate & 3) << 29)
        | (((immediate >> 2) & 0x7ffff) << 5))
}

#[cfg(target_arch = "aarch64")]
fn imm16(insn: u32, immediate: u64) -> u32 {
    (insn & !(0xffff << 5)) | (((immediate as u32) & 0xffff) << 5)
}

#[cfg(target_arch = "aarch64")]
fn imm12(insn: u32, immediate: u64) -> u32 {
    (insn & !(0xfff << 10)) | (((immediate as u32) & 0xfff) << 10)
}

#[cfg(target_arch = "aarch64")]
fn relocate_aarch64(
    _context: &RelocContext<'_>,
    r_type: u32,
    symbol: u64,
    addend: i64,
    place: u64,
    loc: *mut u8,
) -> Result<(), i32> {
    let value = (symbol as i128) + (addend as i128);
    let relative = value - place as i128;
    match r_type {
        256 | 0 => Ok(()),
        257 => {
            write_u64(loc, value as u64);
            Ok(())
        }
        258 => store_unsigned(loc, value, 32),
        259 => store_unsigned(loc, value, 16),
        260 => {
            // R_AARCH64_PREL64 (used by .eh_frame): S + A - P.
            write_u64(loc, relative as u64);
            Ok(())
        }
        261 => store_signed(loc, relative, 32),
        263 | 265 | 267 | 269 => {
            let shift = match r_type {
                263 => 0,
                265 => 16,
                267 => 32,
                _ => 48,
            };
            let immediate = (value >> shift) as u64;
            if immediate > 0xffff {
                return Err(errno(abi::ENOEXEC));
            }
            let insn = read_u32_at(loc);
            write_u32_at(loc, imm16(insn, immediate));
            Ok(())
        }
        264 | 266 | 268 => {
            let shift = match r_type {
                264 => 0,
                266 => 16,
                _ => 32,
            };
            let immediate = (value >> shift) as u64;
            let insn = read_u32_at(loc);
            write_u32_at(loc, imm16(insn, immediate));
            Ok(())
        }
        273 => {
            if relative & 3 != 0 {
                return Err(errno(abi::ENOEXEC));
            }
            let immediate = (relative >> 2) as i64;
            if immediate < -(1i64 << 20) || immediate >= (1i64 << 20) {
                return Err(errno(abi::ENOEXEC));
            }
            let insn = read_u32_at(loc);
            write_u32_at(
                loc,
                (insn & !(0x7ffff << 5)) | (((immediate as u32) & 0x7ffff) << 5),
            );
            Ok(())
        }
        274 => {
            let insn = read_u32_at(loc);
            write_u32_at(loc, adr_imm(insn, relative as i64)?);
            Ok(())
        }
        275 | 276 => {
            let page = (value as u64) & !0xfff;
            let pc_page = place & !0xfff;
            let immediate = (page.wrapping_sub(pc_page) as i64) >> 12;
            let insn = read_u32_at(loc);
            write_u32_at(loc, adr_imm(insn, immediate)?);
            Ok(())
        }
        277 | 278 | 284 | 285 | 286 | 299 => {
            let shift = match r_type {
                278 => 0,
                284 => 1,
                285 => 2,
                286 => 3,
                299 => 4,
                _ => 0,
            };
            let immediate = (value as u64) >> shift;
            let insn = read_u32_at(loc);
            write_u32_at(loc, imm12(insn, immediate));
            Ok(())
        }
        279 => {
            if relative & 3 != 0 {
                return Err(errno(abi::ENOEXEC));
            }
            let immediate = (relative >> 2) as i64;
            if immediate < -(1i64 << 15) || immediate >= (1i64 << 15) {
                return Err(errno(abi::ENOEXEC));
            }
            let insn = read_u32_at(loc);
            write_u32_at(
                loc,
                (insn & !(0x3fff << 5)) | (((immediate as u32) & 0x3fff) << 5),
            );
            Ok(())
        }
        280 => {
            if relative & 3 != 0 {
                return Err(errno(abi::ENOEXEC));
            }
            let immediate = (relative >> 2) as i64;
            if immediate < -(1i64 << 20) || immediate >= (1i64 << 20) {
                return Err(errno(abi::ENOEXEC));
            }
            let insn = read_u32_at(loc);
            write_u32_at(
                loc,
                (insn & !(0x7ffff << 5)) | (((immediate as u32) & 0x7ffff) << 5),
            );
            Ok(())
        }
        282 | 283 => {
            if relative & 3 != 0 {
                return Err(errno(abi::ENOEXEC));
            }
            let delta = relative >> 2;
            if delta < -(1i128 << 25) || delta >= (1i128 << 25) {
                return Err(errno(abi::ENOEXEC));
            }
            let insn = read_u32_at(loc);
            write_u32_at(loc, (insn & !0x03ff_ffff) | ((delta as u32) & 0x03ff_ffff));
            Ok(())
        }
        _ => {
            crate::kwarn!("module: unsupported aarch64 relocation {}", r_type);
            Err(errno(abi::ENOEXEC))
        }
    }
}

#[cfg(target_arch = "riscv64")]
fn fits_signed(value: i64, bits: u32) -> bool {
    value >= -(1i64 << (bits - 1)) && value < (1i64 << (bits - 1))
}

#[cfg(target_arch = "riscv64")]
fn riscv_hi20(value: i64) -> i64 {
    (value + 0x800) & !0xfff
}

#[cfg(target_arch = "riscv64")]
fn riscv_lo12_target(
    hi20: &[(u64, u64, u64)],
    section_va: u64,
    symbol_value: u64,
) -> Result<i64, i32> {
    let relative = symbol_value.wrapping_sub(section_va);
    for &(offset, target, place) in hi20 {
        if offset == relative {
            return Ok(target.wrapping_sub(place) as i64);
        }
    }
    Err(errno(abi::ENOEXEC))
}

#[cfg(target_arch = "riscv64")]
fn relocate_riscv64(
    context: &RelocContext<'_>,
    hi20: &[(u64, u64, u64)],
    r_type: u32,
    symbol: u64,
    addend: i64,
    place: u64,
    loc: *mut u8,
) -> Result<(), i32> {
    let value = (symbol as i128) + (addend as i128);
    match r_type {
        0 | 51 => Ok(()),
        2 => {
            write_u64(loc, value as u64);
            Ok(())
        }
        1 => store_unsigned(loc, value, 32),
        16 => {
            let relative = (symbol.wrapping_add(addend as u64)).wrapping_sub(place) as i64;
            if relative & 1 != 0 || !fits_signed(relative, 13) {
                return Err(errno(abi::ENOEXEC));
            }
            let insn = read_u32_at(loc);
            let patched = (insn & 0x1fff_07f)
                | (((relative >> 12) & 1) as u32) << 31
                | (((relative >> 5) & 0x3f) as u32) << 25
                | (((relative >> 1) & 0xf) as u32) << 8
                | (((relative >> 11) & 1) as u32) << 7;
            write_u32_at(loc, patched);
            Ok(())
        }
        17 => {
            let relative = (symbol.wrapping_add(addend as u64)).wrapping_sub(place) as i64;
            if relative & 1 != 0 || !fits_signed(relative, 21) {
                return Err(errno(abi::ENOEXEC));
            }
            let insn = read_u32_at(loc);
            let patched = (insn & 0xfff)
                | (((relative >> 20) & 1) as u32) << 31
                | (((relative >> 1) & 0x3ff) as u32) << 21
                | (((relative >> 11) & 1) as u32) << 20
                | (((relative >> 12) & 0xff) as u32) << 12;
            write_u32_at(loc, patched);
            Ok(())
        }
        18 | 19 => {
            ensure_bounds(place - context.section_va, 8, context.section_size)?;
            let relative = (symbol.wrapping_add(addend as u64)).wrapping_sub(place) as i64;
            if relative < -(1i64 << 31) - (1i64 << 11) || relative >= (1i64 << 31) - (1i64 << 11) {
                return Err(errno(abi::ENOEXEC));
            }
            let hi = riscv_hi20(relative);
            let lo = relative - hi;
            let insn = read_u32_at(loc);
            write_u32_at(loc, (insn & 0xfff) | (hi as u32));
            // SAFETY: ensure_bounds proved that r_offset + 8 is inside the section.
            let second_loc = unsafe { loc.add(4) };
            let second = read_u32_at(second_loc);
            write_u32_at(
                second_loc,
                (second & 0xfffff) | (((lo as u32) & 0xfff) << 20),
            );
            Ok(())
        }
        23 => {
            let relative = (symbol.wrapping_add(addend as u64)).wrapping_sub(place) as i64;
            if !fits_signed(relative, 32) {
                return Err(errno(abi::ENOEXEC));
            }
            let hi = riscv_hi20(relative);
            let insn = read_u32_at(loc);
            write_u32_at(loc, (insn & 0xfff) | (hi as u32));
            Ok(())
        }
        24 | 25 => {
            let relative = riscv_lo12_target(hi20, context.section_va, symbol)?;
            let hi = riscv_hi20(relative);
            let lo = relative - hi;
            let insn = read_u32_at(loc);
            if r_type == 24 {
                write_u32_at(loc, (insn & 0xfffff) | (((lo as u32) & 0xfff) << 20));
            } else {
                let patched = (insn & 0x1fff_07f)
                    | (((lo >> 5) & 0x7f) as u32) << 25
                    | ((lo & 0x1f) as u32) << 7;
                write_u32_at(loc, patched);
            }
            Ok(())
        }
        26 => {
            let hi = riscv_hi20(value as i64);
            let insn = read_u32_at(loc);
            write_u32_at(loc, (insn & 0xfff) | (hi as u32));
            Ok(())
        }
        27 | 28 => {
            let absolute = value as i64;
            let hi = riscv_hi20(absolute);
            let lo = absolute - hi;
            let insn = read_u32_at(loc);
            if r_type == 27 {
                write_u32_at(loc, (insn & 0xfffff) | (((lo as u32) & 0xfff) << 20));
            } else {
                let patched = (insn & 0x1fff_07f)
                    | (((lo >> 5) & 0x7f) as u32) << 25
                    | ((lo & 0x1f) as u32) << 7;
                write_u32_at(loc, patched);
            }
            Ok(())
        }
        43 => {
            crate::kwarn!("module: R_RISCV_ALIGN requires -mno-relax");
            Err(errno(abi::ENOEXEC))
        }
        _ => {
            crate::kwarn!("module: unsupported riscv64 relocation {}", r_type);
            Err(errno(abi::ENOEXEC))
        }
    }
}

#[cfg(target_arch = "x86_64")]
fn count_got_relocations(elf: &Elf<'_>, loaded: &[Option<usize>]) -> Result<usize, i32> {
    let mut count = 0usize;
    for original in 0..elf.shnum {
        let shdr = elf.shdr(original)?;
        if shdr.kind != SHT_RELA {
            continue;
        }
        let Some(Some(_)) = loaded.get(shdr.info as usize) else {
            continue;
        };
        for index in 0..(shdr.size / 24) as usize {
            let offset = shdr.offset as usize + index * 24;
            let r_type = (read_u64(elf.data, offset + 8)? & 0xffff_ffff) as u32;
            if matches!(r_type, 9 | 41 | 42) {
                count += 1;
            }
        }
    }
    Ok(count)
}

#[cfg(target_arch = "riscv64")]
fn collect_hi20(
    elf: &Elf<'_>,
    rela: &Shdr,
    section_va: u64,
    symtab: &Shdr,
    strtab: &Shdr,
    loaded: &[Option<usize>],
    sections: &[Section],
    cache: &mut [Option<u64>],
    count: usize,
) -> Result<Vec<(u64, u64, u64)>, i32> {
    let mut out = Vec::new();
    for index in 0..count {
        let offset = rela.offset as usize + index * 24;
        let r_offset = read_u64(elf.data, offset)?;
        let r_info = read_u64(elf.data, offset + 8)?;
        let r_type = (r_info & 0xffff_ffff) as u32;
        if r_type != 23 && r_type != 29 {
            continue;
        }
        let symbol_index = (r_info >> 32) as usize;
        let target = resolve_symbol(elf, symtab, strtab, loaded, sections, cache, symbol_index)?;
        let addend = read_u64(elf.data, offset + 16)? as i64;
        out.push((
            r_offset,
            target.wrapping_add(addend as u64),
            section_va + r_offset,
        ));
    }
    Ok(out)
}

fn apply_relocations(
    elf: &Elf<'_>,
    sections: &[Section],
    loaded: &[Option<usize>],
    symtab: &Shdr,
    strtab: &Shdr,
    cache: &mut [Option<u64>],
) -> Result<(), i32> {
    let symtab_index = elf_symtab_index(elf)?;
    let got_state = sections
        .iter()
        .find(|section| section.name == ".spk_got")
        .map(|section| GotState {
            va: section.va,
            phys: section.phys,
            size: section.size,
            next: core::cell::Cell::new(0),
        });
    let got = got_state.as_ref();
    for original in 0..elf.shnum {
        let shdr = elf.shdr(original)?;
        if shdr.kind != SHT_RELA {
            continue;
        }
        if shdr.link as usize != symtab_index {
            return Err(errno(abi::ENOEXEC));
        }
        let target = shdr.info as usize;
        // Relocations against discarded sections (debug info, unwind tables)
        // do not describe loaded memory and cannot affect the module.
        let Some(Some(section_index)) = loaded.get(target) else {
            continue;
        };
        let section = &sections[*section_index];
        if shdr.entsize != 24 {
            return Err(errno(abi::ENOEXEC));
        }
        let count = (shdr.size / 24) as usize;
        if count > MAX_RELOCATIONS {
            return Err(errno(abi::ENOEXEC));
        }
        let context = RelocContext {
            elf,
            rela: shdr,
            section_va: section.va,
            section_size: section.size as u64,
            symbols: symtab,
            strtab,
            loaded,
            sections,
            got,
        };
        #[cfg(target_arch = "riscv64")]
        let hi20 = collect_hi20(
            elf, &shdr, section.va, symtab, strtab, loaded, sections, cache, count,
        )?;
        #[cfg(not(target_arch = "riscv64"))]
        let hi20: &[(u64, u64, u64)] = &[];
        #[cfg(not(target_arch = "riscv64"))]
        let _ = hi20;
        for index in 0..count {
            let offset = shdr.offset as usize + index * 24;
            let r_offset = read_u64(elf.data, offset)?;
            let r_info = read_u64(elf.data, offset + 8)?;
            let r_type = (r_info & 0xffff_ffff) as u32;
            let addend = read_u64(elf.data, offset + 16)? as i64;
            let symbol_index = (r_info >> 32) as usize;
            let symbol =
                resolve_symbol(elf, symtab, strtab, loaded, sections, cache, symbol_index)?;
            let place = section
                .va
                .checked_add(r_offset)
                .ok_or(errno(abi::ENOEXEC))?;
            let location = section
                .phys
                .checked_add(r_offset)
                .ok_or(errno(abi::ENOEXEC))?;
            #[cfg(target_arch = "x86_64")]
            {
                let width = match r_type {
                    0 => 0,
                    1 | 24 => 8,
                    _ => 4,
                };
                ensure_bounds(r_offset, width, section.size as u64).map_err(|error| {
                    crate::kwarn!(
                        "module: relocation {} at {:#x} exceeds section {}",
                        r_type,
                        r_offset,
                        section.name
                    );
                    error
                })?;
                relocate_x86_64(&context, r_type, symbol, addend, place, phys_ptr(location))
                    .map_err(|error| {
                        crate::kwarn!(
                            "module: x86_64 relocation {} at {:#x} in {} failed",
                            r_type,
                            r_offset,
                            section.name
                        );
                        error
                    })?;
            }
            #[cfg(target_arch = "aarch64")]
            {
                let width = match r_type {
                    256 => 0,
                    257 | 260 => 8,
                    259 => 2,
                    _ => 4,
                };
                ensure_bounds(r_offset, width, section.size as u64)?;
                relocate_aarch64(&context, r_type, symbol, addend, place, phys_ptr(location))
                    .map_err(|error| {
                        crate::kwarn!(
                            "module: aarch64 relocation {} at {:#x} in {} failed",
                            r_type,
                            r_offset,
                            section.name
                        );
                        error
                    })?;
            }
            #[cfg(target_arch = "riscv64")]
            {
                let width = match r_type {
                    0 | 43 | 51 => 0,
                    2 => 8,
                    18 | 19 => 8,
                    _ => 4,
                };
                ensure_bounds(r_offset, width, section.size as u64)?;
                relocate_riscv64(
                    &context,
                    &hi20,
                    r_type,
                    symbol,
                    addend,
                    place,
                    phys_ptr(location),
                )
                .map_err(|error| {
                    crate::kwarn!(
                        "module: riscv64 relocation {} at {:#x} in {} failed",
                        r_type,
                        r_offset,
                        section.name
                    );
                    error
                })?;
            }
        }
    }
    Ok(())
}

fn elf_symtab_index(elf: &Elf<'_>) -> Result<usize, i32> {
    for index in 0..elf.shnum {
        if elf.shdr(index)?.kind == SHT_SYMTAB {
            return Ok(index);
        }
    }
    Err(errno(abi::ENOEXEC))
}

fn collect_symbols(
    elf: &Elf<'_>,
    symtab: &Shdr,
    strtab: &Shdr,
    loaded: &[Option<usize>],
    sections: &[Section],
) -> Result<Vec<ModuleSymbol>, i32> {
    let count = (symtab.size / 24) as usize;
    if count > MAX_SYMBOLS {
        return Err(errno(abi::ENOEXEC));
    }
    let mut out = Vec::new();
    for index in 0..count {
        let symbol = elf.symbol(symtab, index)?;
        let kind = symbol.info & 0xf;
        let binding = symbol.info >> 4;
        if matches!(kind, STT_SECTION | STT_FILE | STT_TLS)
            || !matches!(kind, STT_FUNC | STT_OBJECT)
        {
            continue;
        }
        if symbol.shndx == SHN_UNDEF || symbol.shndx >= SHN_LORESERVE {
            continue;
        }
        let name = elf.string(strtab, symbol.name)?;
        if name.is_empty()
            || name.starts_with(".L")
            || name.starts_with('$')
            || name.len() > MAX_NAME
        {
            continue;
        }
        let Some(Some(section_index)) = loaded.get(symbol.shndx as usize) else {
            continue;
        };
        let section = &sections[*section_index];
        let Some(address) = section.va.checked_add(symbol.value) else {
            continue;
        };
        let class = section.class;
        let upper = binding != 0 && binding != STB_WEAK;
        let kind = match (class, upper) {
            (Class::Text, true) => b'T',
            (Class::Text, false) => b't',
            (Class::RoData, true) => b'R',
            (Class::RoData, false) => b'r',
            (Class::Data, true) => b'D',
            (Class::Data, false) => b'd',
            (Class::Bss, true) => b'B',
            (Class::Bss, false) => b'b',
        };
        out.push(ModuleSymbol {
            name: name.to_string(),
            address,
            kind,
        });
    }
    Ok(out)
}

fn parse_metadata(
    sections: &[Section],
) -> Result<(String, String, String, InitFn, Option<ExitFn>), i32> {
    let section = sections
        .iter()
        .find(|section| section.name == ".spk_module")
        .ok_or_else(|| {
            crate::kwarn!("module: missing .spk_module metadata section");
            errno(abi::ENOEXEC)
        })?;
    if section.size < METADATA_SIZE {
        crate::kwarn!(
            "module: .spk_module is {} bytes, need {}",
            section.size,
            METADATA_SIZE
        );
        return Err(errno(abi::ENOEXEC));
    }
    let magic = read_phys_u32(section.phys, 0);
    let abi = read_phys_u32(section.phys, 4);
    if magic != SPK_MODULE_MAGIC {
        crate::kwarn!("module: bad metadata magic {:#x}", magic);
        return Err(errno(abi::ENOEXEC));
    }
    if abi != SPK_MODULE_ABI_VERSION {
        crate::kwarn!(
            "module: ABI version {} does not match kernel ABI {}",
            abi,
            SPK_MODULE_ABI_VERSION
        );
        return Err(errno(abi::ENOEXEC));
    }
    let name = module_string(sections, read_phys_u64(section.phys, 8), 64)?;
    let version = module_string(sections, read_phys_u64(section.phys, 16), 64)?;
    let license = module_string(sections, read_phys_u64(section.phys, 24), 64)?;
    let vermagic = module_string(sections, read_phys_u64(section.phys, 32), 128)?;
    let init = read_phys_u64(section.phys, 40);
    let exit = read_phys_u64(section.phys, 48);
    if name.is_empty()
        || name.len() > 63
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        crate::kwarn!("module: invalid module name");
        return Err(errno(abi::EINVAL));
    }
    if vermagic != crate::module::vermagic() {
        crate::kwarn!(
            "module: vermagic mismatch (module '{}', kernel '{}')",
            vermagic,
            crate::module::vermagic()
        );
        return Err(errno(abi::ENOEXEC));
    }
    if init == 0 {
        crate::kwarn!("module: metadata init pointer is null");
        return Err(errno(abi::ENOEXEC));
    }
    // SAFETY: function pointers originate from the relocated module image and
    // point at its executable section.
    let init: InitFn = unsafe { core::mem::transmute(init) };
    let exit: Option<ExitFn> = if exit == 0 {
        None
    } else {
        // SAFETY: same contract as the init pointer.
        Some(unsafe { core::mem::transmute(exit) })
    };
    Ok((name, version, license, init, exit))
}

pub fn load(image: &[u8]) -> Result<PendingModule, i32> {
    if image.is_empty() || image.len() as u64 > MAX_MODULE_BYTES {
        return Err(errno(abi::ENOEXEC));
    }
    let elf = match Elf::parse(image) {
        Ok(elf) => elf,
        Err(error) => {
            crate::kwarn!("module: ELF validation failed ({})", error);
            return Err(error);
        }
    };
    let symtab_index = match elf_symtab_index(&elf) {
        Ok(index) => index,
        Err(error) => {
            crate::kwarn!("module: no usable symbol table ({})", error);
            return Err(error);
        }
    };
    let symtab = elf.shdr(symtab_index)?;
    let strtab = elf.shdr(symtab.link as usize)?;
    if strtab.kind != SHT_STRTAB {
        return Err(errno(abi::ENOEXEC));
    }

    let mut inputs: Vec<Input> = Vec::new();
    let mut loaded = alloc::vec![None; elf.shnum];
    for index in 1..elf.shnum {
        let shdr = elf.shdr(index)?;
        let name = elf.section_name(&shdr)?;
        if let Some(class) = classify(name, &shdr)? {
            if shdr.addralign > 4096 || !shdr.addralign.is_power_of_two() {
                crate::kwarn!("module: section {} has invalid alignment", name);
                return Err(errno(abi::ENOEXEC));
            }
            if shdr.kind != SHT_NOBITS {
                elf.bytes(shdr.offset, shdr.size)?;
            }
            if shdr.size > MAX_MODULE_BYTES {
                return Err(errno(abi::ENOEXEC));
            }
            loaded[index] = Some(inputs.len());
            inputs.push(Input {
                name: name.to_string(),
                header: shdr,
                class,
                offset: 0,
                pages: 0,
                synthetic: false,
            });
        }
    }
    if inputs.is_empty() {
        return Err(errno(abi::ENOEXEC));
    }

    #[cfg(target_arch = "x86_64")]
    {
        let slots = count_got_relocations(&elf, &loaded)?;
        if slots != 0 {
            let size = (slots as u64).checked_mul(8).ok_or(errno(abi::ENOMEM))?;
            inputs.push(Input {
                name: String::from(".spk_got"),
                header: Shdr {
                    name: 0,
                    kind: SHT_PROGBITS,
                    flags: SHF_ALLOC | SHF_WRITE,
                    offset: 0,
                    size,
                    link: 0,
                    info: 0,
                    addralign: 8,
                    entsize: 0,
                },
                class: Class::Data,
                offset: 0,
                pages: 0,
                synthetic: true,
            });
        }
    }

    let mut cursor = 0u64;
    for class in [Class::Text, Class::RoData, Class::Data, Class::Bss] {
        for input in inputs.iter_mut().filter(|input| input.class == class) {
            cursor = align_up(cursor, 4096).ok_or(errno(abi::ENOMEM))?;
            input.offset = cursor;
            let pages = (input.header.size.div_ceil(4096)) as usize;
            input.pages = pages;
            cursor = cursor
                .checked_add(pages as u64 * 4096)
                .ok_or(errno(abi::ENOMEM))?;
        }
    }
    let total_pages = (cursor / 4096 + 1) as usize;
    let base = crate::module::virt::allocate(total_pages)?;

    let mut sections = Vec::with_capacity(inputs.len());
    for input in &inputs {
        sections.push(Section {
            name: input.name.clone(),
            va: base + input.offset,
            phys: 0,
            pages: input.pages,
            size: input.header.size as usize,
            class: input.class,
        });
    }

    let mut failure: Option<i32> = None;
    for (index, input) in inputs.iter().enumerate() {
        let section = &mut sections[index];
        if section.pages == 0 {
            continue;
        }
        let Some(physical) = pmm::alloc(section.pages) else {
            failure = Some(errno(abi::ENOMEM));
            break;
        };
        section.phys = physical;
        if let Err(error) = vmm::map_kernel(
            section.va,
            physical,
            section.pages,
            input.class.protection(),
        ) {
            crate::kwarn!(
                "module: mapping section {} failed: {:?}",
                section.name,
                error
            );
            pmm::free(physical, section.pages).expect("module frame rollback failed");
            section.phys = 0;
            failure = Some(errno(abi::ENOMEM));
            break;
        }
        // SAFETY: the frames are exclusively owned by this module and the HHDM
        // alias is writable. Zero padding as well as BSS.
        unsafe {
            ptr::write_bytes(phys_ptr(physical), 0, section.pages * 4096);
        }
        if !input.synthetic && input.header.kind != SHT_NOBITS && input.header.size != 0 {
            let source = elf.bytes(input.header.offset, input.header.size)?;
            // SAFETY: the destination window is exactly section.size bytes.
            unsafe {
                ptr::copy_nonoverlapping(source.as_ptr(), phys_ptr(physical), source.len());
            }
        }
    }
    if let Some(error) = failure {
        release_sections(&sections);
        crate::module::virt::deallocate(base, total_pages);
        return Err(error);
    }

    let mut cache = alloc::vec![None; (symtab.size / 24) as usize];
    if let Err(error) = apply_relocations(&elf, &sections, &loaded, &symtab, &strtab, &mut cache) {
        crate::kwarn!("module: relocation failed ({})", error);
        release_sections(&sections);
        crate::module::virt::deallocate(base, total_pages);
        return Err(error);
    }

    for section in &sections {
        if section.class == Class::Text && section.pages != 0 {
            crate::arch::sync_instruction_cache(section.va, section.pages * 4096);
        }
    }

    let (name, version, license, init, exit) = match parse_metadata(&sections) {
        Ok(metadata) => metadata,
        Err(error) => {
            release_sections(&sections);
            crate::module::virt::deallocate(base, total_pages);
            return Err(error);
        }
    };
    let symbols = match collect_symbols(&elf, &symtab, &strtab, &loaded, &sections) {
        Ok(symbols) => symbols,
        Err(error) => {
            release_sections(&sections);
            crate::module::virt::deallocate(base, total_pages);
            return Err(error);
        }
    };
    let total_size = total_pages * 4096;
    Ok(PendingModule {
        name,
        version,
        license,
        vermagic: crate::module::vermagic(),
        init,
        exit,
        sections,
        symbols,
        total_size,
        va: base,
        pages: total_pages,
    })
}
