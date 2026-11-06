/*
 *
 *       modules/hello-rust/src/lib.rs
 *       Rust loadable module example
 *
 *       2026/11/6 By JiTianyu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

#![no_std]

#[path = "../../sdk/rust/spk.rs"]
mod spk;

use core::ffi::{c_char, c_int};

unsafe extern "C" fn hello_rust_init() -> c_int {
    spk::print("hello-rust: Hello, world from a Rust SpaceKernel module!\n");
    spk::print("hello-rust: init through the Rust SDK\n");
    spk::print("HELLO_RUST_MODULE_OK\n");
    0
}

unsafe extern "C" fn hello_rust_exit() {
    spk::print("hello-rust: module unloaded\n");
}

#[used]
#[unsafe(link_section = ".spk_module")]
static MODULE: spk::SpkModule = spk::SpkModule {
    magic: spk::SPK_MODULE_MAGIC,
    abi_version: spk::SPK_MODULE_ABI_VERSION,
    name: c"hello-rust".as_ptr(),
    version: c"1.0".as_ptr(),
    license: c"GPL".as_ptr(),
    vermagic: spk::VERMAGIC_C.as_ptr() as *const c_char,
    init: Some(hello_rust_init),
    exit: Some(hello_rust_exit),
};

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    spk::error("hello-rust: panic\n");
    loop {
        core::hint::spin_loop();
    }
}
