//
//       user/spk/spk-core/src/lib.rs
//       SpaceOS-9 package manager core: schemas, versions, signatures, archives
//
//       2026/11/20 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

//! Shared core types for the SpaceOS-9 package manager.
//!
//! `spk-core` is used by both the target-side `spk` binary and the build-host
//! `spk-host` tool. It deliberately avoids C dependencies: package creation
//! (zstd encoding) lives in `spk-host`, while this crate only decodes zstd
//! through `ruzstd`.

pub mod archive;
pub mod db;
pub mod error;
pub mod fsutil;
pub mod index;
pub mod keys;
pub mod manifest;
pub mod store;
pub mod version;

pub use error::{Error, Result};
