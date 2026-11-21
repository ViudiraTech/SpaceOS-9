/*
 *       src/user/stack.rs
 *       Bounded cross-page System V initial stack and auxiliary vectors
 *       2026/10/24 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 */
use super::{
    address_space::{ProcessImage, UserAddressSpace},
    elf::LoadedElf,
};
use alloc::{string::String, vec, vec::Vec};
pub const USER_STACK_PAGES: usize = 64;
pub fn stack_top() -> u64 {
    UserAddressSpace::user_limit() - 0x10000
}

fn push(
    space: &UserAddressSpace,
    sp: &mut u64,
    base: u64,
    bytes: &[u8],
) -> Result<u64, &'static str> {
    *sp = sp
        .checked_sub(bytes.len() as u64)
        .filter(|s| *s >= base)
        .ok_or("arguments exceed user stack")?;
    space
        .initialize(*sp, bytes)
        .map_err(|_| "stack copy failed")?;
    Ok(*sp)
}
fn strings(
    space: &UserAddressSpace,
    sp: &mut u64,
    base: u64,
    items: &[&str],
) -> Result<(Vec<u64>, Vec<u8>), &'static str> {
    if items.len() > 4096 {
        return Err("too many arguments");
    }
    let mut pointers = Vec::new();
    let mut flat = Vec::new();
    for item in items {
        if item.as_bytes().contains(&0) {
            return Err("NUL in arguments");
        }
        let mut value = item.as_bytes().to_vec();
        value.push(0);
        pointers.push(push(space, sp, base, &value)?);
        flat.extend_from_slice(&value);
    }
    Ok((pointers, flat))
}
pub fn setup_user_stack(
    space: &mut UserAddressSpace,
    loaded: &LoadedElf,
    name: &str,
) -> Result<u64, &'static str> {
    setup(space, loaded, name, &[name], &[], false)
}
pub fn setup(
    space: &mut UserAddressSpace,
    loaded: &LoadedElf,
    name: &str,
    argv: &[&str],
    env: &[&str],
    secure: bool,
) -> Result<u64, &'static str> {
    let top = stack_top();
    let base = top - USER_STACK_PAGES as u64 * 4096;
    // The page below base is deliberately absent and never used for payload.
    if space.contains(base - 4096) {
        return Err("ELF overlaps stack guard");
    }
    space.map_zeroed(base, top - base, 3)?;
    let result = (|| {
        let mut sp = top;
        let (args, argv_flat) = strings(space, &mut sp, base, argv)?;
        let (envp, env_flat) = strings(space, &mut sp, base, env)?;
        let mut executable = name.as_bytes().to_vec();
        executable.push(0);
        let execfn = push(space, &mut sp, base, &executable)?;
        #[cfg(target_arch = "x86_64")]
        let platform_str = b"x86_64\0";
        #[cfg(target_arch = "aarch64")]
        let platform_str = b"aarch64\0";
        #[cfg(target_arch = "riscv64")]
        let platform_str = b"riscv64\0";
        let platform = push(space, &mut sp, base, platform_str)?;
        let mut aux = vec![
            (3, loaded.phdr),
            (4, 56),
            (5, loaded.phnum as u64),
            (6, 4096),
            (7, loaded.interpreter_base),
            (8, 0),
            (9, loaded.entry),
            (11, 0),
            (12, 0),
            (13, 0),
            (14, 0),
            (15, platform),
            (17, 100),
            (23, u64::from(secure)),
            (31, execfn),
        ];
        let mut random = [0; 16];
        if crate::random::fill(&mut random).is_ok() {
            let p = push(space, &mut sp, base, &random)?;
            aux.push((25, p));
        }
        aux.push((0, 0));
        let mut values = vec![args.len() as u64];
        values.extend(args);
        values.push(0);
        values.extend(envp);
        values.push(0);
        let mut auxbytes = Vec::new();
        for &(key, value) in &aux {
            values.extend([key, value]);
            auxbytes.extend_from_slice(&key.to_le_bytes());
            auxbytes.extend_from_slice(&value.to_le_bytes());
        }
        let length = values.len().checked_mul(8).ok_or("stack vector overflow")?;
        sp = sp
            .checked_sub(length as u64)
            .filter(|&v| v >= base)
            .ok_or("stack overflow")?
            & !15;
        if sp < base {
            return Err("stack alignment overflow");
        }
        for (index, value) in values.iter().enumerate() {
            space
                .initialize(sp + index as u64 * 8, &value.to_le_bytes())
                .map_err(|_| "stack vector copy failed")?;
        }
        space.image = Some(ProcessImage {
            executable: String::from(name),
            argv: argv_flat,
            environ: env_flat,
            auxv: auxbytes,
        });
        Ok(sp)
    })();
    if result.is_err() {
        let _ = space.unmap(base, top - base);
    }
    result
}
