/*
 *
 *       src/hal/mod.rs
 *       Driver-facing hardware abstraction layer for loadable modules
 *
 *       2026/11/15 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! The HAL is the only interface a loadable driver uses (see docs/HAL.md and
//! `modules/include/spk/module.h`). It has three layers:
//!
//! * Bus and I/O: PCI discovery and configuration, MSI/MSI-X and INTx,
//!   coherent and streaming DMA, MMIO and port I/O (`module::hw`).
//! * Execution context: heap, time, spinlocks, timers, work items, kernel
//!   threads, and the sleeping primitives in [`sync`].
//! * Device classes: drivers publish what they find through class
//!   interfaces — network (`net::module_api`), sound (`sound`), [`input`] and
//!   [`block`] — and never touch the subsystems' internals.
//!
//! All entry points are plain C ABI functions exported by
//! `module::api::EXPORTS`. [`HAL_VERSION`] only grows; additions never change
//! an existing call, so `spk_hal_version() >= N` is a sufficient check.

pub mod block;
pub mod input;
pub mod sync;

/// Bumped whenever calls are added to the HAL.
pub const HAL_VERSION: u32 = 1;

pub extern "C" fn spk_hal_version() -> u32 {
    HAL_VERSION
}
