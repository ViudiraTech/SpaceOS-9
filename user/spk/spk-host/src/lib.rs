//
//       user/spk/spk-host/src/lib.rs
//       spk-host library: pack, repository index generation, verification
//
//       2026/11/20 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

pub mod indexgen;
pub mod pack;

pub use indexgen::{
    generate, generate_with_keys, generate_with_keys_and_security, verify_index, verify_repo,
    IndexResult, SecurityConfig, VerifyReport,
};
pub use pack::{pack, PackResult};
