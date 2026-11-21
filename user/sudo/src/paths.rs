//
//       user/sudo/src/paths.rs
//       System and test-root paths used by sudo/doas
//
//       2026/11/21 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct Paths {
    pub etc: PathBuf,
    pub run: PathBuf,
    pub var: PathBuf,
}

impl Paths {
    pub fn system() -> Self {
        Self {
            etc: PathBuf::from("/etc"),
            run: PathBuf::from("/run"),
            var: PathBuf::from("/var"),
        }
    }

    pub fn rooted(root: &Path) -> Self {
        Self {
            etc: root.join("etc"),
            run: root.join("run"),
            var: root.join("var"),
        }
    }

    pub fn passwd(&self) -> PathBuf {
        self.etc.join("passwd")
    }
    pub fn group(&self) -> PathBuf {
        self.etc.join("group")
    }
    pub fn shadow(&self) -> PathBuf {
        self.etc.join("shadow")
    }
    pub fn sudoers(&self) -> PathBuf {
        self.etc.join("sudoers")
    }
    pub fn doas_conf(&self) -> PathBuf {
        self.etc.join("doas.conf")
    }
    pub fn ticket_dir(&self) -> PathBuf {
        self.run.join("sudo/ts")
    }
    pub fn ticket(&self, uid: u32) -> PathBuf {
        self.ticket_dir().join(uid.to_string())
    }
    pub fn faillock_dir(&self) -> PathBuf {
        self.run.join("sudo/faillock")
    }
    pub fn faillock(&self, uid: u32) -> PathBuf {
        self.faillock_dir().join(uid.to_string())
    }
    pub fn lectured_dir(&self) -> PathBuf {
        self.var.join("lib/sudo/lectured")
    }
    pub fn lectured(&self, uid: u32) -> PathBuf {
        self.lectured_dir().join(uid.to_string())
    }
    pub fn audit(&self) -> PathBuf {
        self.var.join("log/sudo.log")
    }
}
