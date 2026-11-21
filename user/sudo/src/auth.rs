//
//       user/sudo/src/auth.rs
//       /etc/shadow authentication: sha512-crypt, sha256-crypt, yescrypt
//
//       2026/11/21 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

use std::fmt;

use crate::config::shadow_hash;
use crate::paths::Paths;

#[derive(Debug)]
pub enum AuthError {
    Missing,
    Locked,
    Unsupported(String),
    BadPassword,
    Io(String),
}

impl fmt::Display for AuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing => f.write_str("no shadow entry"),
            Self::Locked => f.write_str("account is locked"),
            Self::Unsupported(kind) => write!(f, "unsupported password hash {kind}"),
            Self::BadPassword => f.write_str("incorrect password"),
            Self::Io(error) => f.write_str(error),
        }
    }
}

impl std::error::Error for AuthError {}

pub fn verify_password(paths: &Paths, user: &str, password: &str) -> Result<(), AuthError> {
    let hash = shadow_hash(paths, user)
        .map_err(AuthError::Io)?
        .ok_or(AuthError::Missing)?;
    if hash.is_empty() {
        // OOBE may leave an account with an empty password. Only an empty
        // password is accepted; `!`/`*` remain explicitly locked.
        return if password.is_empty() {
            Ok(())
        } else {
            Err(AuthError::BadPassword)
        };
    }
    if hash.starts_with('!') || hash.starts_with('*') {
        return Err(AuthError::Locked);
    }
    let password = password.as_bytes();
    if hash.starts_with("$6$") {
        <sha_crypt::ShaCrypt as sha_crypt::password_hash::PasswordVerifier<str>>::verify_password(
            &sha_crypt::ShaCrypt::SHA512,
            password,
            hash.as_str(),
        )
        .map_err(|_| AuthError::BadPassword)
    } else if hash.starts_with("$5$") {
        <sha_crypt::ShaCrypt as sha_crypt::password_hash::PasswordVerifier<str>>::verify_password(
            &sha_crypt::ShaCrypt::SHA256,
            password,
            hash.as_str(),
        )
        .map_err(|_| AuthError::BadPassword)
    } else if hash.starts_with("$y$") {
        <yescrypt::Yescrypt as yescrypt::password_hash::PasswordVerifier<str>>::verify_password(
            &yescrypt::Yescrypt::default(),
            password,
            hash.as_str(),
        )
        .map_err(|_| AuthError::BadPassword)
    } else {
        Err(AuthError::Unsupported(
            hash.chars().take(3).collect::<String>(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config;
    use crate::paths::Paths;
    use std::fs;

    fn root_with_shadow(hash: &str) -> (tempfile::TempDir, Paths) {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted(dir.path());
        fs::create_dir_all(&paths.etc).unwrap();
        fs::write(
            paths.passwd(),
            "root:x:0:0:root:/root:/bin/sh\nalice:x:1000:1000:Alice:/home/alice:/bin/sh\n",
        )
        .unwrap();
        fs::write(
            paths.shadow(),
            format!("root:!::0:::::\nalice:{hash}:20000:0:99999:7:::\n"),
        )
        .unwrap();
        (dir, paths)
    }

    #[test]
    fn verifies_sha512_crypt() {
        use sha_crypt::password_hash::PasswordHasher;
        let hash = sha_crypt::ShaCrypt::SHA512
            .hash_password_with_salt(b"secret", b"testsalt")
            .unwrap()
            .to_string();
        let (_dir, paths) = root_with_shadow(&hash);
        assert!(verify_password(&paths, "alice", "secret").is_ok());
        assert!(matches!(
            verify_password(&paths, "alice", "wrong"),
            Err(AuthError::BadPassword)
        ));
    }

    #[test]
    fn verifies_yescrypt() {
        use yescrypt::password_hash::PasswordHasher;
        let hash = yescrypt::Yescrypt::default()
            .hash_password(b"secret")
            .unwrap()
            .to_string();
        assert!(hash.starts_with("$y$"));
        let (_dir, paths) = root_with_shadow(&hash);
        assert!(verify_password(&paths, "alice", "secret").is_ok());
        assert!(matches!(
            verify_password(&paths, "alice", "wrong"),
            Err(AuthError::BadPassword)
        ));
    }

    #[test]
    fn rejects_locked_and_unknown_hashes() {
        let (_dir, paths) = root_with_shadow("");
        assert!(verify_password(&paths, "alice", "").is_ok());
        assert!(matches!(
            verify_password(&paths, "alice", "x"),
            Err(AuthError::BadPassword)
        ));
        let (_dir, paths) = root_with_shadow("!locked");
        assert!(matches!(
            verify_password(&paths, "alice", "x"),
            Err(AuthError::Locked)
        ));
        let (_dir, paths) = root_with_shadow("$1$legacy");
        assert!(matches!(
            verify_password(&paths, "alice", "x"),
            Err(AuthError::Unsupported(_))
        ));
        let (_dir, paths) = root_with_shadow("$y$bad");
        assert!(matches!(
            verify_password(&paths, "alice", "x"),
            Err(AuthError::BadPassword)
        ));
        let (_dir, paths) = root_with_shadow("$6$bad");
        assert!(matches!(
            verify_password(&paths, "alice", "x"),
            Err(AuthError::BadPassword)
        ));
        let _ = config::DEFAULT_SECURE_PATH;
    }
}
