/*
 *
 *       src/arch/paging.rs
 *       Page-table geometry shared by all supported architectures
 *
 *       2026/9/30 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Page-table geometry shared by all supported architectures.

/// Permissions for kernel mappings created after boot, such as loadable
/// module sections. User mappings use their own per-architecture encoding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KernelProt {
    ReadOnly,
    ReadOnlyExec,
    ReadWrite,
    ReadWriteExec,
}

impl KernelProt {
    pub const fn writable(self) -> bool {
        matches!(self, Self::ReadWrite | Self::ReadWriteExec)
    }

    pub const fn executable(self) -> bool {
        matches!(self, Self::ReadOnlyExec | Self::ReadWriteExec)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Geometry {
    pub levels: usize,
    pub virtual_bits: u32,
    pub physical_bits: u32,
}

impl Geometry {
    pub const fn slot_bytes(self) -> u64 {
        1 << (12 + 9 * (self.levels - 1))
    }

    /// Sign extend the upper-half root index into a canonical kernel address.
    pub fn slot_base(self, index: usize) -> u64 {
        let address = index as u64 * self.slot_bytes();
        let shift = 64 - self.virtual_bits;
        (((address << shift) as i64) >> shift) as u64
    }

    pub fn valid_physical(self, address: u64) -> bool {
        address < (1 << self.physical_bits)
    }
}
