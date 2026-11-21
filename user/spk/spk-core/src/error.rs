//
//       user/spk/spk-core/src/error.rs
//       Unified error type for spk core
//
//       2026/11/20 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

use std::io;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Io(#[from] io::Error),
    #[error("invalid manifest: {0}")]
    Manifest(String),
    #[error("invalid index: {0}")]
    Index(String),
    #[error("invalid package: {0}")]
    Package(String),
    #[error("unsafe path: {0}")]
    UnsafePath(String),
    #[error("invalid key: {0}")]
    Key(String),
    #[error("signature verification failed: {0}")]
    Signature(String),
    #[error("invalid version: {0}")]
    Version(String),
    #[error("invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid TOML: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("TOML serialization failed: {0}")]
    TomlSer(#[from] toml::ser::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
