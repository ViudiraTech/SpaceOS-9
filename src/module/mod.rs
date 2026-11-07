/*
 *
 *       src/module/mod.rs
 *       Module registry, load/unload lifecycle and /proc views
 *
 *       2026/11/6 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Loadable kernel module registry.
//!
//! Load and unload are serialized by a cooperative gate.  Module code is never
//! executed while the registry lock is held, and init runs before the module
//! becomes visible to `/proc` and `/sys`.

pub mod api;
mod elf;
pub mod hw;
#[cfg(feature = "boot-self-test")]
pub mod self_test;
mod sysfs;
pub(crate) mod virt;

use alloc::{
    format,
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::{kinfo, kwarn, sched, sync::SpinLock, time, vfs::abi};

pub const ABI_VERSION: u32 = 2;
const MAX_PARAMS: usize = 1024;

struct LoadedModule {
    name: String,
    version: String,
    license: String,
    vermagic: String,
    params: String,
    sections: Vec<elf::Section>,
    symbols: Vec<elf::ModuleSymbol>,
    total_size: usize,
    va: u64,
    pages: usize,
    exit: Option<elf::ExitFn>,
    loaded_us: u64,
    refcount: AtomicUsize,
}

impl LoadedModule {
    fn address(&self) -> u64 {
        self.sections
            .iter()
            .find(|section| section.pages != 0)
            .map_or(0, |section| section.va)
    }
}

#[derive(Clone)]
pub struct ModuleInfo {
    pub name: String,
    pub version: String,
    pub license: String,
    pub size: usize,
    pub refcount: usize,
    pub address: u64,
}

static MODULES: SpinLock<Vec<Arc<LoadedModule>>> = SpinLock::new(Vec::new());
static OPERATION_BUSY: AtomicBool = AtomicBool::new(false);

struct OperationGuard;

impl OperationGuard {
    fn acquire() -> Self {
        loop {
            if !OPERATION_BUSY.swap(true, Ordering::Acquire) {
                return Self;
            }
            // Module init/exit may sleep through spk_msleep; yield instead of
            // spinning with interrupts disabled.
            if sched::current().is_some() {
                sched::sleep_micros(500);
            } else {
                core::hint::spin_loop();
            }
        }
    }
}

impl Drop for OperationGuard {
    fn drop(&mut self) {
        OPERATION_BUSY.store(false, Ordering::Release);
    }
}

/// UTS release, machine and SMP marker checked against every module.
pub fn vermagic() -> String {
    format!("{} {} SMP", crate::spk::release(), crate::spk::UTS_MACHINE)
}

pub fn init() {
    sysfs::install();
    kinfo!(
        "Modules: ABI {} vermagic '{}', {} exported symbols",
        ABI_VERSION,
        vermagic(),
        api::EXPORTS.len()
    );
}

fn privileged() -> bool {
    sched::current().is_none_or(|task| task.credentials().euid == 0)
}

/// Load a module image.  Returns a negative errno on failure.
pub fn load(image: &[u8], params: &str) -> Result<(), i32> {
    if !privileged() {
        return Err(-(abi::EPERM as i32));
    }
    if params.len() > MAX_PARAMS {
        return Err(-(abi::EINVAL as i32));
    }
    let _guard = OperationGuard::acquire();
    let mut pending = elf::load(image)?;
    {
        let modules = MODULES.lock();
        if modules.iter().any(|module| module.name == pending.name) {
            drop(modules);
            pending.release();
            return Err(-(abi::EEXIST as i32));
        }
    }

    api::set_current_name(&pending.name);
    kinfo!(
        "module: loading {} version {} (license {}, {} bytes)",
        pending.name,
        pending.version,
        pending.license,
        pending.total_size
    );
    // SAFETY: elf::load validated the metadata, mapped executable text and
    // completed every relocation before returning the entry pointer.
    let result = unsafe { (pending.init)() };
    api::clear_current_name();
    if result != 0 {
        let code = if result < 0 {
            result
        } else {
            -(abi::EINVAL as i32)
        };
        kwarn!("module: {} init failed with {}", pending.name, result);
        pending.release();
        return Err(code);
    }

    let elf::PendingModule {
        name,
        version,
        license,
        vermagic,
        init: _,
        exit,
        sections,
        symbols,
        total_size,
        va,
        pages,
    } = pending;
    kinfo!("module: {} loaded", name);
    let module = Arc::new(LoadedModule {
        name,
        version,
        license,
        vermagic,
        params: params.to_string(),
        sections,
        symbols,
        total_size,
        va,
        pages,
        exit,
        loaded_us: time::uptime_micros(),
        refcount: AtomicUsize::new(0),
    });
    MODULES.lock().push(module);
    Ok(())
}

/// Unload a module by name.  `force` permits a nonzero reference count.
pub fn unload(name: &str, force: bool) -> Result<(), i32> {
    if !privileged() {
        return Err(-(abi::EPERM as i32));
    }
    let _guard = OperationGuard::acquire();
    let module = {
        let mut modules = MODULES.lock();
        let Some(index) = modules.iter().position(|module| module.name == name) else {
            return Err(-(abi::ENOENT as i32));
        };
        if !force && modules[index].refcount.load(Ordering::Acquire) != 0 {
            return Err(-(abi::EBUSY as i32));
        }
        modules.remove(index)
    };
    api::set_current_name(&module.name);
    if let Some(exit) = module.exit {
        // SAFETY: the module is unpublished, no other thread can enter its
        // code, and the image is still mapped.
        unsafe { exit() };
    }
    api::clear_current_name();
    elf::release_module(&module.sections, module.va, module.pages);
    kinfo!(
        "module: {} unloaded after {} us",
        module.name,
        time::uptime_micros().saturating_sub(module.loaded_us)
    );
    Ok(())
}

pub fn is_loaded(name: &str) -> bool {
    MODULES.lock().iter().any(|module| module.name == name)
}

pub fn names() -> Vec<String> {
    MODULES
        .lock()
        .iter()
        .map(|module| module.name.clone())
        .collect()
}

/// Snapshot used by procfs and sysfs.  The registry lock is released before
/// strings are allocated.
pub fn snapshot() -> Vec<ModuleInfo> {
    let modules: Vec<Arc<LoadedModule>> = MODULES.lock().clone();
    modules
        .iter()
        .map(|module| ModuleInfo {
            name: module.name.clone(),
            version: module.version.clone(),
            license: module.license.clone(),
            size: module.total_size,
            refcount: module.refcount.load(Ordering::Acquire),
            address: module.address(),
        })
        .collect()
}

pub fn info(name: &str) -> Option<ModuleInfo> {
    snapshot().into_iter().find(|module| module.name == name)
}

pub fn params(name: &str) -> Option<String> {
    let modules: Vec<Arc<LoadedModule>> = MODULES.lock().clone();
    modules
        .iter()
        .find(|module| module.name == name)
        .map(|module| module.params.clone())
}

/// Append `address type name [module]` lines for every loaded module.
pub fn append_kallsyms(out: &mut alloc::string::String) {
    let modules: Vec<Arc<LoadedModule>> = MODULES.lock().clone();
    for module in modules {
        for symbol in &module.symbols {
            out.push_str(&format!(
                "{:016x} {} {} [{}]\n",
                symbol.address, symbol.kind as char, symbol.name, module.name
            ));
        }
    }
}

/// `/proc/modules` content in Linux's field order.
pub fn proc_modules() -> String {
    let mut out = String::new();
    for module in snapshot() {
        out.push_str(&format!(
            "{} {} {} - Live {:#018x}\n",
            module.name, module.size, module.refcount, module.address
        ));
    }
    out
}
