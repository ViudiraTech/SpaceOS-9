/*
 *
 *       src/module/self_test.rs
 *       Boot-time loadable module integration test
 *
 *       2026/11/6 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Loads the C and Rust hello modules from build artifacts embedded by
//! `build.rs`, checks the procfs/sysfs views, then unloads them.

static HELLO: &[u8] = include_bytes!(env!("SPK_HELLO_KO"));
static HELLO_RUST: &[u8] = include_bytes!(env!("SPK_HELLO_RUST_KO"));

fn check_module(name: &str) {
    assert!(crate::module::is_loaded(name), "module {name} missing");
    let modules = crate::vfs::read_file("/proc/modules").expect("/proc/modules unavailable");
    assert!(
        modules
            .windows(name.len())
            .any(|window| window == name.as_bytes()),
        "/proc/modules does not list {name}"
    );
    let refcnt = crate::vfs::read_file(&alloc::format!("/sys/module/{name}/refcnt"))
        .expect("/sys/module attribute unavailable");
    assert_eq!(refcnt, b"0\n");
}

pub fn run() {
    if HELLO.is_empty() {
        crate::kwarn!("module self-test: hello.ko was not built by the Makefile");
        return;
    }
    crate::module::load(HELLO, "").expect("C hello module failed to load");
    check_module("hello");
    #[cfg(feature = "kallsyms")]
    assert!(
        crate::kallsyms::has_symbol("spk_printk"),
        "generated kallsyms is missing an exported symbol"
    );
    #[cfg(feature = "module-unload")]
    {
        crate::module::unload("hello", false).expect("C hello module failed to unload");
        assert!(!crate::module::is_loaded("hello"));
    }

    if HELLO_RUST.is_empty() {
        crate::kwarn!("module self-test: hello-rust.ko was not built by the Makefile");
    } else {
        crate::module::load(HELLO_RUST, "").expect("Rust hello module failed to load");
        check_module("hello-rust");
        #[cfg(feature = "module-unload")]
        {
            crate::module::unload("hello-rust", false).expect("Rust hello module failed to unload");
            assert!(!crate::module::is_loaded("hello-rust"));
        }
    }
    crate::verbose::log(format_args!("loadable module self-test passed"));
}
