//
//       user/spk/spk-host/src/lib.rs
//       spk-host library: pack, repository index generation, verification
//
//       2026/11/20 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

pub mod indexgen;
pub mod pack;

pub use indexgen::{generate, verify_index, verify_repo, IndexResult, VerifyReport};
pub use pack::{pack, PackResult};
