/*
 *
 *       src/arch/riscv64/cpuid.rs
 *       Supervisor-safe identity discovery using SBI BASE and the selected CPU DT node
 *
 *       2026/9/30 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Supervisor-safe identity discovery using SBI BASE and the selected CPU DT node.

use crate::{
    boot,
    cpuid::{Architecture, CpuInfo, Feature, Features},
    hardware::fdt::Fdt,
};
use core::arch::asm;

/// SBI BASE getters return an error on old firmware. Never read misa,
/// mvendorid, marchid or mimpid directly from supervisor mode.
fn sbi_identity(function: usize) -> Option<u64> {
    let (error, value): (isize, u64);
    // SAFETY: SBI BASE is a read-only firmware call; unsupported returns error.
    unsafe {
        asm!("ecall", inlateout("a0") 0isize => error, lateout("a1") value,
            in("a6") function, in("a7") 0x10usize, options(nostack));
    }
    (error == 0).then_some(value)
}

fn extension(features: &mut Features, name: &str) {
    let feature = match name {
        "i" => Feature::RiscvI,
        "m" => Feature::RiscvM,
        "a" => Feature::RiscvA,
        "c" => Feature::RiscvC,
        "f" => Feature::RiscvF,
        "d" => Feature::RiscvD,
        "v" => Feature::RiscvV,
        "zkr" => Feature::RiscvZkr,
        _ => return,
    };
    features.insert(feature);
}

/// Decode the legacy ISA string without mistaking named Z extensions for
/// single-letter extensions. Version numbers belong to the preceding letter.
pub(crate) fn parse_isa(isa: &str) -> Features {
    let mut features = Features::default();
    if let Some(rest) = isa.strip_prefix("rv64")
        && matches!(rest.as_bytes().first(), Some(b'i' | b'g' | b'e'))
    {
        for name in rest.split('_').skip(1) {
            let tail = name.get(1..).unwrap_or("");
            if tail.bytes().all(|b| b.is_ascii_digit() || b == b'p') {
                extension(&mut features, name.get(..1).unwrap_or(""));
            } else if name.starts_with('z') {
                // Named extension with an optional version, e.g. "zkr1p0".
                let base = name.trim_end_matches(|c: char| c.is_ascii_digit() || c == 'p');
                extension(&mut features, base);
            }
        }
        for byte in rest
            .split('_')
            .next()
            .unwrap_or("")
            .bytes()
            .take_while(|b| !matches!(b, b'z' | b's' | b'x'))
        {
            if byte == b'g' {
                for name in ["i", "m", "a", "f", "d"] {
                    extension(&mut features, name);
                }
            } else if byte.is_ascii_lowercase() && byte != b'p' {
                extension(&mut features, core::str::from_utf8(&[byte]).unwrap_or(""));
            }
        }
    }
    features
}

pub fn detect(index: usize) -> CpuInfo {
    let mut info = CpuInfo::empty(Architecture::Riscv64);
    info.hardware_id = boot::cpu_hardware_id(index).expect("unknown hart");
    info.vendor.set(b"RISC-V");
    info.vendor_id = sbi_identity(4); // mvendorid
    info.signature = sbi_identity(5).unwrap_or(0); // marchid
    info.revision_id = sbi_identity(6); // mimpid
    info.model.set(b"RV64 (firmware identity)");
    info.virtual_bits = Some(super::paging::paging_geometry().virtual_bits as u8);
    // Physical PPN encoding is not a report of implemented physical width.
    if let Some(tree) = boot::dtb_address().and_then(|addr| Fdt::from_boot_address(addr).ok()) {
        if let Some(cpu) = tree.cpus().find(|cpu| {
            cpu.reg()
                .and_then(|mut regs| regs.next())
                .is_some_and(|reg| reg.address == info.hardware_id)
        }) {
            if let Some(isa) = cpu.property("riscv,isa").and_then(|p| p.as_str()) {
                info.features = parse_isa(isa);
                info.model.set(isa.as_bytes());
            }
            if let Some(extensions) = cpu.property("riscv,isa-extensions") {
                for name in extensions.as_string_list() {
                    extension(&mut info.features, name);
                }
            }
            if cpu.property("riscv,isa-base").and_then(|p| p.as_str()) == Some("rv64i") {
                info.features.insert(Feature::RiscvI);
            }
        }
        info.counter_hz = tree
            .find_node("/cpus")
            .and_then(|cpu| cpu.cell("timebase-frequency"))
            .filter(|n| *n != 0)
            .map(u64::from);
    }
    if info.features.contains(Feature::RiscvF) {
        info.features.insert(Feature::Fpu);
    }
    if info.features.contains(Feature::RiscvD) {
        info.features.insert(Feature::DoublePrecision);
    }
    if info.features.contains(Feature::RiscvV) {
        info.features.insert(Feature::Simd);
    }
    if info.features.contains(Feature::RiscvA) {
        info.features.insert(Feature::Atomics);
    }
    info.features.insert(Feature::Nx);
    info
}
