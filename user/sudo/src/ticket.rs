//
//       user/sudo/src/ticket.rs
//       Timestamp tickets and failed-authentication backoff
//
//       2026/11/21 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

use std::fs;
use std::io;
use std::path::Path;

use crate::paths::Paths;
use crate::util::{ensure_dir, now, read_to_string, remove_if_exists, write_private};

#[derive(Clone, Debug)]
pub struct Ticket {
    pub uid: u32,
    pub tty: String,
    pub expires: u64,
}

fn parse_ticket(text: &str) -> Option<Ticket> {
    let mut uid = None;
    let mut tty = String::new();
    let mut expires = None;
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("uid=") {
            uid = value.parse().ok();
        } else if let Some(value) = line.strip_prefix("tty=") {
            tty = value.to_string();
        } else if let Some(value) = line.strip_prefix("expires=") {
            expires = value.parse().ok();
        }
    }
    Some(Ticket {
        uid: uid?,
        tty,
        expires: expires?,
    })
}

pub fn read_ticket(paths: &Paths, uid: u32) -> Option<Ticket> {
    let text = read_to_string(&paths.ticket(uid)).ok()?;
    let ticket = parse_ticket(&text)?;
    (ticket.uid == uid).then_some(ticket)
}

pub fn valid(paths: &Paths, uid: u32, tty: &str, timeout: u64, global: bool) -> bool {
    if timeout == 0 {
        return false;
    }
    let Some(ticket) = read_ticket(paths, uid) else {
        return false;
    };
    if now() >= ticket.expires {
        return false;
    }
    if !global && !ticket.tty.is_empty() && !tty.is_empty() && ticket.tty != tty {
        return false;
    }
    true
}

pub fn refresh(paths: &Paths, uid: u32, tty: &str, timeout: u64) -> io::Result<()> {
    ensure_dir(&paths.ticket_dir(), 0o700)?;
    let expires = now().saturating_add(timeout);
    let data = format!("uid={uid}\ntty={tty}\nexpires={expires}\n");
    write_private(&paths.ticket(uid), data.as_bytes(), 0o600)
}

pub fn clear(paths: &Paths, uid: u32) -> io::Result<()> {
    remove_if_exists(&paths.ticket(uid))
}

pub fn clear_all(paths: &Paths) -> io::Result<()> {
    let dir = paths.ticket_dir();
    match fs::read_dir(&dir) {
        Ok(entries) => {
            for entry in entries.flatten() {
                if entry
                    .file_type()
                    .map(|kind| kind.is_file())
                    .unwrap_or(false)
                {
                    let _ = fs::remove_file(entry.path());
                }
            }
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Failure {
    pub count: u32,
    pub last: u64,
}

fn read_failure(path: &Path) -> Option<Failure> {
    let text = read_to_string(path).ok()?;
    let mut parts = text.split_whitespace();
    Some(Failure {
        count: parts.next()?.parse().ok()?,
        last: parts.next()?.parse().ok()?,
    })
}

/// Track failures even when no ticket is issued. Once `tries` failures have
/// accumulated, authentication is refused for `lock_seconds`.
pub fn locked(paths: &Paths, uid: u32, tries: u32, lock_seconds: u64) -> bool {
    let Some(failure) = read_failure(&paths.faillock(uid)) else {
        return false;
    };
    failure.count >= tries && now().saturating_sub(failure.last) < lock_seconds
}

pub fn record_failure(paths: &Paths, uid: u32) -> io::Result<u32> {
    ensure_dir(&paths.faillock_dir(), 0o700)?;
    let failure = read_failure(&paths.faillock(uid)).unwrap_or(Failure { count: 0, last: 0 });
    let count = failure.count.saturating_add(1);
    let data = format!("{count} {}\n", now());
    write_private(&paths.faillock(uid), data.as_bytes(), 0o600)?;
    Ok(count)
}

pub fn clear_failures(paths: &Paths, uid: u32) -> io::Result<()> {
    remove_if_exists(&paths.faillock(uid))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn tickets_expire_and_respect_tty() {
        let dir = tempdir().unwrap();
        let paths = Paths::rooted(dir.path());
        refresh(&paths, 1000, "/dev/pts/1", 60).unwrap();
        assert!(valid(&paths, 1000, "/dev/pts/1", 60, false));
        assert!(!valid(&paths, 1000, "/dev/pts/2", 60, false));
        assert!(valid(&paths, 1000, "/dev/pts/2", 60, true));
        assert!(!valid(&paths, 1000, "/dev/pts/1", 0, false));
        clear(&paths, 1000).unwrap();
        assert!(!valid(&paths, 1000, "/dev/pts/1", 60, false));
    }

    #[test]
    fn failure_backoff() {
        let dir = tempdir().unwrap();
        let paths = Paths::rooted(dir.path());
        assert!(!locked(&paths, 1000, 3, 300));
        record_failure(&paths, 1000).unwrap();
        record_failure(&paths, 1000).unwrap();
        assert!(!locked(&paths, 1000, 3, 300));
        record_failure(&paths, 1000).unwrap();
        assert!(locked(&paths, 1000, 3, 300));
        clear_failures(&paths, 1000).unwrap();
        assert!(!locked(&paths, 1000, 3, 300));
    }
}
