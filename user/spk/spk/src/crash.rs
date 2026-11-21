//
//       user/spk/spk/src/crash.rs
//       Deterministic SIGKILL injection for transaction recovery tests
//
//       2026/11/20 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

use std::io::Write;

/// When `SPK_TEST_CRASH` names this point, kill the process with SIGKILL.
/// This mirrors an external `kill -9` and is used only by the QEMU matrix.
pub fn point(name: &str) {
    if std::env::var("SPK_TEST_CRASH").ok().as_deref() != Some(name) {
        return;
    }
    let _ = writeln!(std::io::stderr(), "SPK_TEST_CRASH: hit {name}");
    let _ = std::io::stderr().flush();
    let _ = rustix::process::kill_process(rustix::process::getpid(), rustix::process::Signal::KILL);
    std::process::abort();
}
