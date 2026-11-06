/*
 *
 *       src/syscall/kmod.rs
 *       init_module(2), finit_module(2) and delete_module(2)
 *
 *       2026/11/6 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Linux loadable-module system calls.
//!
//! Module images are copied out of user memory or read from a file descriptor
//! before parsing; the loader never dereferences a user pointer.

use alloc::{string::String, vec, vec::Vec};

use crate::{
    module,
    user::uaccess,
    vfs::{self, VfsError, abi},
};

const MAX_MODULE_BYTES: usize = 64 * 1024 * 1024;
const MAX_MODULE_NAME: usize = 64;

fn parameter_string(pointer: u64) -> Result<String, i64> {
    if pointer == 0 {
        return Ok(String::new());
    }
    uaccess::string(pointer, 1024).map_err(|error| error.to_errno())
}

fn read_image(fd: i32) -> Result<Vec<u8>, i64> {
    if fd < 0 {
        return Err(-abi::EBADF);
    }
    let entry = {
        let table = vfs::current_fd_table();
        let guard = table.lock();
        guard.get(fd as usize)
    }
    .ok_or(-abi::EBADF)?;
    let mut image = Vec::new();
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        match entry.file.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => {
                image.extend_from_slice(&buffer[..count]);
                if image.len() > MAX_MODULE_BYTES {
                    return Err(-abi::ENOMEM);
                }
            }
            Err(VfsError::Interrupted) => continue,
            Err(error) => return Err(error.to_errno()),
        }
    }
    Ok(image)
}

pub fn init_module(image: u64, length: usize, parameters: u64) -> i64 {
    if length == 0 {
        return -abi::EINVAL;
    }
    if length > MAX_MODULE_BYTES {
        return -abi::ENOMEM;
    }
    let image = match uaccess::read_vec(image, length, length) {
        Ok(image) => image,
        Err(error) => return error.to_errno(),
    };
    let parameters = match parameter_string(parameters) {
        Ok(parameters) => parameters,
        Err(error) => return error,
    };
    match module::load(&image, &parameters) {
        Ok(()) => 0,
        Err(error) => error as i64,
    }
}

pub fn finit_module(fd: i32, parameters: u64, flags: u32) -> i64 {
    // MODULE_INIT_IGNORE_MODVERSIONS / IGNORE_VERMAGIC would defeat the ABI
    // and vermagic checks this kernel relies on.
    if flags != 0 {
        return -abi::EINVAL;
    }
    let parameters = match parameter_string(parameters) {
        Ok(parameters) => parameters,
        Err(error) => return error,
    };
    let image = match read_image(fd) {
        Ok(image) => image,
        Err(error) => return error,
    };
    match module::load(&image, &parameters) {
        Ok(()) => 0,
        Err(error) => error as i64,
    }
}

pub fn delete_module(name: u64, flags: u32) -> i64 {
    if name == 0 {
        return -abi::EFAULT;
    }
    let allowed = abi::O_NONBLOCK | abi::O_TRUNC;
    if flags & !allowed != 0 {
        return -abi::EINVAL;
    }
    #[cfg(not(feature = "module-force-unload"))]
    if flags & abi::O_TRUNC != 0 {
        return -abi::EPERM;
    }
    let name = match uaccess::string(name, MAX_MODULE_NAME) {
        Ok(name) => name,
        Err(error) => return error.to_errno(),
    };
    if name.is_empty() {
        return -abi::EINVAL;
    }
    let force = flags & abi::O_TRUNC != 0;
    match module::unload(&name, force) {
        Ok(()) => 0,
        Err(error) => error as i64,
    }
}
