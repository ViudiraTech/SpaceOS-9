//
//       user/sudo/src/lib.rs
//       Shared sudo/doas implementation
//
//       2026/11/21 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

pub mod app;
pub mod auth;
pub mod config;
pub mod exec;
pub mod lecture;
pub mod paths;
pub mod ticket;
pub mod util;

pub use paths::Paths;
