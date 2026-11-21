//
//       user/spk/spk/src/error.rs
//       Target-side error type and plan exit codes
//
//       2026/11/20 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

use std::fmt;

use spk_core::Error as CoreError;

#[derive(Debug)]
pub enum SpkError {
    Usage(String),
    General(String),
    Signature(String),
    Dependency { message: String, lines: Vec<String> },
    FileConflict { message: String, lines: Vec<String> },
    Check { message: String, lines: Vec<String> },
    RolledBack(String),
    Script(String),
    Network(String),
    Security(String),
    Io(std::io::Error),
    Json(serde_json::Error),
}

impl SpkError {
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Usage(_) => 2,
            Self::Signature(_) => 3,
            Self::Dependency { .. } => 4,
            Self::FileConflict { .. } => 5,
            Self::RolledBack(_) | Self::Script(_) => 6,
            _ => 1,
        }
    }

    pub fn lines(&self) -> &[String] {
        match self {
            Self::Dependency { lines, .. }
            | Self::FileConflict { lines, .. }
            | Self::Check { lines, .. } => lines,
            _ => &[],
        }
    }
}

impl fmt::Display for SpkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage(message)
            | Self::General(message)
            | Self::Signature(message)
            | Self::RolledBack(message)
            | Self::Script(message)
            | Self::Network(message)
            | Self::Security(message) => f.write_str(message),
            Self::Dependency { message, .. }
            | Self::FileConflict { message, .. }
            | Self::Check { message, .. } => f.write_str(message),
            Self::Io(error) => write!(f, "{error}"),
            Self::Json(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for SpkError {}

impl From<std::io::Error> for SpkError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for SpkError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

impl From<CoreError> for SpkError {
    fn from(error: CoreError) -> Self {
        match error {
            CoreError::Signature(message) => Self::Signature(message),
            other => Self::General(other.to_string()),
        }
    }
}

pub type Result<T> = std::result::Result<T, SpkError>;
