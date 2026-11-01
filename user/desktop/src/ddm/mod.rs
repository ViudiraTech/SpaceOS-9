/*
 *
 *       user/desktop/src/ddm/mod.rs
 *       DDM display manager daemon: privileged session, authentication and OOBE backend
 *
 *       2026/11/01 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! DDM is the privileged half of the graphical login stack.  It owns the X
//! server, Xauthority cookies, account database transactions and session
//! processes.  The greeter and the OOBE wizard are unprivileged clients that
//! can only submit requests over a Unix socket.
//!
//! The split follows the same trust boundary used by SDDM, LightDM and DDM:
//! the greeter never reads `/etc/shadow`, never starts processes as another
//! user and never receives an X cookie belonging to a logged-in account.

pub mod accounts;
pub mod auth;
pub mod config;
pub mod daemon;
pub mod sessions;
pub mod wire;
pub mod xauth;

pub use daemon::run;
