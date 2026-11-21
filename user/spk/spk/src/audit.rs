//
//       user/spk/spk/src/audit.rs
//       Append-only transaction audit log
//
//       2026/11/23 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::json;

use crate::error::Result;
use crate::state::Paths;
use crate::transaction::OperationKind;

pub fn record(
    paths: &Paths,
    operation: OperationKind,
    packages: &[String],
    result: &str,
) -> Result<()> {
    let directory = paths.root.join("var/log/spk");
    fs::create_dir_all(&directory)?;
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o755))?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o640)
        .open(directory.join("audit.log"))?;
    let line = json!({
        "schema": 1,
        "time": SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs(),
        "uid": unsafe { libc::getuid() },
        "pid": std::process::id(),
        "operation": match operation {
            OperationKind::Install => "install",
            OperationKind::Upgrade => "upgrade",
            OperationKind::Remove => "remove",
        },
        "packages": packages,
        "result": result,
    });
    let mut bytes = serde_json::to_vec(&line)?;
    bytes.push(b'\n');
    file.write_all(&bytes)?;
    file.sync_data()?;
    Ok(())
}
