/*
 *
 *       src/main.rs
 *       Kernel entry and subsystem initialization
 *
 *       2026/9/30 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]
#![cfg_attr(feature = "modules", feature(c_variadic))]
#![allow(dead_code)]
#![cfg_attr(target_arch = "x86_64", feature(abi_x86_interrupt))]

extern crate alloc;

mod arch;
pub mod block;
mod boot;
pub mod cpuid;
pub mod device;
pub mod fpu;
pub mod gpu;
mod hardware;
pub mod input;
pub mod ipc;
pub mod irq;
#[cfg(feature = "kallsyms")]
mod kallsyms;
mod mm;
#[cfg(feature = "modules")]
pub mod module;
pub mod pci;
pub mod printk;
mod random;
pub mod sched;
#[cfg(feature = "boot-self-test")]
pub mod self_test;
pub mod shell;
pub mod signal;
mod smp;
pub mod spk;
mod sync;
pub mod syscall;
mod time;
pub mod tty;
pub mod user;
pub mod verbose;
pub mod vfs;

use core::panic::PanicInfo;

#[unsafe(no_mangle)]
pub extern "C" fn kmain() -> ! {
    arch::disable_interrupts();
    #[cfg(target_arch = "x86_64")]
    arch::serial_init(0);
    let info = boot::read();
    cpuid::init();
    mm::pmm::init(boot::memory_map(), info.hhdm);
    mm::vmm::init();
    #[cfg(not(target_arch = "x86_64"))]
    {
        let uart = mm::vmm::map_device_page(arch::SERIAL_PHYS).expect("UART mapping failed");
        arch::serial_init(uart);
    }
    let prepared_cpus = arch::init_exceptions();
    arch::install_cpu_index(boot::bsp_cpu_index());
    fpu::init().expect("extended CPU state initialization failed");
    time::init();
    tty::init();
    kinfo!("SpaceKernel: Limine entry");
    kinfo!("{}", spk::banner());
    let cpu = cpuid::info().expect("CPU discovery required");
    kinfo!(
        "CPU: {:?} {} {} id={:#x} PA={:?} VA={:?}",
        cpu.architecture,
        cpu.vendor.as_str(),
        cpu.model.as_str(),
        cpu.hardware_id,
        cpu.physical_bits,
        cpu.virtual_bits
    );
    kinfo!("FPU: {:?}", fpu::config());
    kinfo!(
        "paging: {} levels, {} virtual bits",
        arch::paging_geometry().levels,
        arch::paging_geometry().virtual_bits
    );
    kinfo!("exception state prepared for {} CPUs", prepared_cpus);
    kinfo!("kernel_cmdline: {}", boot::cmdline());
    let console = tty::status();
    kinfo!(
        "console: serial={} fbcon={} tty{} {}x{}",
        console.serial,
        console.framebuffer,
        console.active_vt,
        console.columns,
        console.rows
    );
    if console.invalid_argument {
        kwarn!("invalid console= argument ignored");
    }
    if console.framebuffer_unavailable {
        kwarn!("framebuffer console unavailable; using ttyS0");
    }
    kinfo!(
        "HHDM={:#x} kernel={:#x} phys={:#x}",
        info.hhdm,
        info.kernel_virtual,
        info.kernel_physical
    );
    let stats = mm::pmm::stats();
    kinfo!("PMM: {} free / {} frames", stats.free, stats.frames);
    kinfo!("VMM root={:#x}", mm::vmm::root().unwrap());
    kinfo!(
        "RSDP={:?} DTB={:?}",
        boot::rsdp_address(),
        boot::dtb_address()
    );
    hardware::report();
    #[cfg(target_arch = "x86_64")]
    let interrupt_ready = match arch::apic::init_bsp() {
        Ok(()) => true,
        Err(error) => {
            kwarn!("APIC unavailable: {error:?}");
            false
        }
    };
    #[cfg(target_arch = "aarch64")]
    let interrupt_ready = match arch::gic::init_bsp() {
        Ok(()) => true,
        Err(error) => {
            kwarn!("GIC unavailable: {error:?}");
            false
        }
    };
    #[cfg(target_arch = "riscv64")]
    let interrupt_ready = match arch::plic::init_bsp() {
        Ok(()) => {
            let _ = arch::clint::init_bsp();
            true
        }
        Err(error) => {
            kwarn!("PLIC unavailable: {error:?}");
            false
        }
    };
    match pci::init() {
        Ok(count) => {
            kinfo!(
                "PCI: discovered {} functions in {} ECAM windows",
                count,
                pci::segments().len()
            );
            for device in pci::devices() {
                kinfo!(
                    "PCI {} {:04x}:{:04x} class {:02x}:{:02x}:{:02x}",
                    device.address,
                    device.vendor_id,
                    device.device_id,
                    device.class,
                    device.subclass,
                    device.programming_interface
                );
            }
        }
        Err(error) => kwarn!("PCI discovery unavailable: {error:?}"),
    }

    assert!(interrupt_ready, "scheduler requires interrupt controller");
    arch::serial_enable_irq();
    hardware::ps2::init();
    block::init();
    vfs::init();
    #[cfg(feature = "modules")]
    module::init();
    gpu::init();
    input::init();
    #[cfg(feature = "boot-self-test")]
    self_test::run_early();
    sched::init(boot::cpu_count());
    smp::start();
    kinfo!("Scheduler: per-CPU EEVDF ready");
    vfs::start_services();
    match boot::init_path() {
        Some(path) => match vfs::read_file(&path) {
            Ok(image) => match user::spawn_init(&image, &path) {
                Ok(pid) => kinfo!("init started: {} (pid {})", path, pid),
                Err(error) => kwarn!("init {} could not start: {}", path, error),
            },
            Err(_) => kwarn!("init image not found: {}", path),
        },
        None => shell::start(),
    }
    gpu::start();
    #[cfg(feature = "boot-self-test")]
    self_test::spawn_late();
    #[cfg(not(feature = "boot-self-test"))]
    kinfo!("BOOT_OK");
    smp::release();
    sched::start_current_cpu();
    sched::idle_loop()
}

#[alloc_error_handler]
fn allocation_error(layout: core::alloc::Layout) -> ! {
    panic!("kernel heap allocation failed: {layout:?}")
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    arch::disable_interrupts();
    printk::emergency(format_args!("PANIC: {info}"));
    loop {
        arch::halt();
    }
}
