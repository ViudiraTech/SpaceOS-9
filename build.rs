/*
 *
 *       build.rs
 *       Locate module artifacts for boot self-tests
 *
 *       2026/11/6 By JiTianyu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! The Makefile builds `hello.ko` and `hello-rust.ko` before invoking Cargo.
//! `cargo check` in a clean tree has no build directory, so an empty object in
//! OUT_DIR keeps the crate compiling while a misbuilt boot image fails its
//! self-test loudly instead of silently skipping the check.

use std::{env, fs, path::PathBuf};

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let out = PathBuf::from(env::var("OUT_DIR").expect("out dir"));
    let modules = env::var("SPK_MODULE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| manifest.join("build/modules/unknown"));
    let stub = out.join("missing-module.ko");
    if !stub.is_file() {
        fs::write(&stub, []).expect("write module stub");
    }
    println!("cargo:rerun-if-env-changed=SPK_MODULE_DIR");
    for (variable, file) in [
        ("SPK_HELLO_KO", "hello.ko"),
        ("SPK_HELLO_RUST_KO", "hello-rust.ko"),
    ] {
        let candidate = modules.join(file);
        let path = if candidate.is_file() {
            candidate
        } else {
            stub.clone()
        };
        println!("cargo:rerun-if-changed={}", path.display());
        println!("cargo:rustc-env={}={}", variable, path.display());
    }
}
