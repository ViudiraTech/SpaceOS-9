/*
 *
 *       src/module/sysfs.rs
 *       /sys/module topology backed by the live module registry
 *
 *       2026/11/6 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Dynamic `/sys/module/<name>/<attribute>` view.

use alloc::{format, string::String, sync::Arc, vec::Vec};

use crate::vfs::{DirEntry, Inode, InodeKind, Stat, VfsError, tree::Attribute};

const ATTRIBUTES: &[&str] = &[
    "refcnt",
    "initstate",
    "version",
    "licenses",
    "holders",
    "taint",
];

fn ino_for(name: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for &byte in name.as_bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash | 1
}

struct ModuleRoot;

impl Inode for ModuleRoot {
    fn metadata(&self) -> Result<Stat, VfsError> {
        Ok(Stat::simple(
            ino_for("sys/module"),
            InodeKind::Directory,
            0o555,
            0,
        ))
    }

    fn lookup(&self, name: &str) -> Result<Arc<dyn Inode>, VfsError> {
        if !crate::module::is_loaded(name) {
            return Err(VfsError::NotFound);
        }
        Ok(Arc::new(ModuleDirectory {
            name: String::from(name),
        }))
    }

    fn readdir(&self) -> Result<Vec<DirEntry>, VfsError> {
        let id = ino_for("sys/module");
        let mut entries = alloc::vec![
            DirEntry {
                ino: id,
                name: String::from("."),
                kind: InodeKind::Directory,
            },
            DirEntry {
                ino: id,
                name: String::from(".."),
                kind: InodeKind::Directory,
            },
        ];
        for name in crate::module::names() {
            entries.push(DirEntry {
                ino: ino_for(&name),
                name,
                kind: InodeKind::Directory,
            });
        }
        Ok(entries)
    }
}

struct ModuleDirectory {
    name: String,
}

impl Inode for ModuleDirectory {
    fn metadata(&self) -> Result<Stat, VfsError> {
        if !crate::module::is_loaded(&self.name) {
            return Err(VfsError::NotFound);
        }
        Ok(Stat::simple(
            ino_for(&self.name),
            InodeKind::Directory,
            0o555,
            0,
        ))
    }

    fn lookup(&self, name: &str) -> Result<Arc<dyn Inode>, VfsError> {
        if !crate::module::is_loaded(&self.name) {
            return Err(VfsError::NotFound);
        }
        if !ATTRIBUTES.contains(&name) {
            return Err(VfsError::NotFound);
        }
        let module_name = self.name.clone();
        let attribute = String::from(name);
        Ok(Attribute::new(0o444, move || {
            let info = crate::module::info(&module_name).ok_or(VfsError::NotFound)?;
            let value = match attribute.as_str() {
                "refcnt" => format!("{}\n", info.refcount),
                "initstate" => String::from("live\n"),
                "version" => format!("{}\n", info.version),
                "licenses" => format!("{}\n", info.license),
                "holders" => String::from("\n"),
                "taint" => String::from("0\n"),
                _ => return Err(VfsError::NotFound),
            };
            Ok(value.into_bytes())
        }))
    }

    fn readdir(&self) -> Result<Vec<DirEntry>, VfsError> {
        if !crate::module::is_loaded(&self.name) {
            return Err(VfsError::NotFound);
        }
        let id = ino_for(&self.name);
        let mut entries = alloc::vec![
            DirEntry {
                ino: id,
                name: String::from("."),
                kind: InodeKind::Directory,
            },
            DirEntry {
                ino: id,
                name: String::from(".."),
                kind: InodeKind::Directory,
            },
        ];
        for attribute in ATTRIBUTES {
            entries.push(DirEntry {
                ino: ino_for(attribute),
                name: String::from(*attribute),
                kind: InodeKind::Regular,
            });
        }
        Ok(entries)
    }
}

pub fn install() {
    let root = crate::device::sys_root();
    if root.insert("module", Arc::new(ModuleRoot)).is_err() {
        crate::kwarn!("sysfs: /sys/module already present");
    }
}
