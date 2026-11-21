/*
 *
 *       user/desktop/src/ime/mod.rs
 *       SPIME daemon, X11 VortexGlass/Win98 candidate popup window, and XIM server.
 *
 *       2026/11/20 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

pub mod daemon;
pub mod popup;
pub mod xim;

pub use daemon::run;
