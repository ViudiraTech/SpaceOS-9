/*
 *
 *       user/desktop/src/ddm/auth.rs
 *       Shadow password verification, password policy and account metadata
 *
 *       2026/11/01 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Authentication is deliberately self-contained instead of pulling PAM into
//! the first SpaceOS release.  It implements the same checks a PAM `account`
//! and `auth` stack performs for local accounts: account existence, shadow
//! lock, password aging, expiry, minimum UID policy and crypt(3) verification.
//! Failed attempts are rate limited in `daemon.rs` and written to the audit log.

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_long, c_ulong};
use std::path::Path;

const CRYPT_ALPHABET: &[u8] = b"./0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
const SHADOW_BUFFER: usize = 4096;
const PASSWD_BUFFER: usize = 4096;

// musl's public shadow.h exposes only getspnam_r; declaring the same layout
// here also lets the module compile against older libc crate versions.
#[repr(C)]
struct Spwd {
    sp_namp: *mut c_char,
    sp_pwdp: *mut c_char,
    sp_lstchg: c_long,
    sp_min: c_long,
    sp_max: c_long,
    sp_warn: c_long,
    sp_inact: c_long,
    sp_expire: c_long,
    sp_flag: c_ulong,
}

extern "C" {
    fn crypt(key: *const c_char, salt: *const c_char) -> *mut c_char;
    fn getspnam_r(
        name: *const c_char,
        spbuf: *mut Spwd,
        buf: *mut c_char,
        buflen: usize,
        spbufp: *mut *mut Spwd,
    ) -> c_int;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UserRecord {
    pub name: String,
    pub uid: u32,
    pub gid: u32,
    pub gecos: String,
    pub home: String,
    pub shell: String,
}

impl UserRecord {
    pub fn display_name(&self) -> String {
        let full = self
            .gecos
            .split(',')
            .next()
            .unwrap_or_default()
            .trim()
            .to_string();
        if full.is_empty() {
            self.name.clone()
        } else {
            full
        }
    }

    pub fn is_login_shell(&self) -> bool {
        !matches!(
            self.shell.as_str(),
            "" | "/sbin/nologin"
                | "/usr/sbin/nologin"
                | "/bin/nologin"
                | "/bin/false"
                | "/usr/bin/false"
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShadowRecord {
    pub hash: String,
    pub last_change: i64,
    pub minimum: i64,
    pub maximum: i64,
    pub warning: i64,
    pub inactive: i64,
    pub expire: i64,
    pub flag: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthError {
    /// Deliberately indistinguishable from an unknown account.
    Denied,
    Expired,
    MustChange,
    Disabled,
    Policy,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthFailure {
    pub error: AuthError,
    pub message: &'static str,
}

impl AuthFailure {
    fn denied() -> Self {
        Self {
            error: AuthError::Denied,
            message: "用户名或密码错误",
        }
    }
}

pub fn lookup_user(name: &str) -> Option<UserRecord> {
    if name.len() > 32 || name.as_bytes().contains(&0) {
        return None;
    }
    let name = CString::new(name).ok()?;
    let mut storage: Vec<u8> = vec![0; PASSWD_BUFFER];
    loop {
        let mut record: libc::passwd = unsafe { std::mem::zeroed() };
        let mut result: *mut libc::passwd = std::ptr::null_mut();
        let rc = unsafe {
            libc::getpwnam_r(
                name.as_ptr(),
                &mut record,
                storage.as_mut_ptr() as *mut c_char,
                storage.len(),
                &mut result,
            )
        };
        if rc == libc::ERANGE && storage.len() < 64 * 1024 {
            storage.resize(storage.len() * 2, 0);
            continue;
        }
        if rc != 0 || result.is_null() {
            return None;
        }
        return Some(UserRecord {
            name: unsafe { cstr(record.pw_name) },
            uid: record.pw_uid,
            gid: record.pw_gid,
            gecos: unsafe { cstr(record.pw_gecos) },
            home: unsafe { cstr(record.pw_dir) },
            shell: unsafe { cstr(record.pw_shell) },
        });
    }
}

pub fn lookup_shadow(name: &str) -> Option<ShadowRecord> {
    if name.len() > 32 || name.as_bytes().contains(&0) {
        return None;
    }
    let name = CString::new(name).ok()?;
    let mut storage: Vec<u8> = vec![0; SHADOW_BUFFER];
    loop {
        let mut record = Spwd {
            sp_namp: std::ptr::null_mut(),
            sp_pwdp: std::ptr::null_mut(),
            sp_lstchg: 0,
            sp_min: 0,
            sp_max: 0,
            sp_warn: 0,
            sp_inact: 0,
            sp_expire: 0,
            sp_flag: 0,
        };
        let mut result: *mut Spwd = std::ptr::null_mut();
        let rc = unsafe {
            getspnam_r(
                name.as_ptr(),
                &mut record,
                storage.as_mut_ptr() as *mut c_char,
                storage.len(),
                &mut result,
            )
        };
        if rc == libc::ERANGE && storage.len() < 64 * 1024 {
            storage.resize(storage.len() * 2, 0);
            continue;
        }
        if rc != 0 || result.is_null() {
            return None;
        }
        return Some(ShadowRecord {
            hash: unsafe { cstr(record.sp_pwdp) },
            last_change: record.sp_lstchg as i64,
            minimum: record.sp_min as i64,
            maximum: record.sp_max as i64,
            warning: record.sp_warn as i64,
            inactive: record.sp_inact as i64,
            expire: record.sp_expire as i64,
            flag: record.sp_flag as u64,
        });
    }
}

/// All human accounts that may appear in the greeter.  System accounts,
/// `nologin` users and the greeter itself are excluded here, not in the UI.
pub fn human_users(minimum_uid: u32) -> Vec<UserRecord> {
    let Ok(text) = std::fs::read_to_string("/etc/passwd") else {
        return Vec::new();
    };
    let mut users = Vec::new();
    for line in text.lines() {
        let fields: Vec<&str> = line.split(':').collect();
        if fields.len() < 7 || fields[0].starts_with('#') {
            continue;
        }
        let Ok(uid) = fields[2].parse::<u32>() else {
            continue;
        };
        let Ok(gid) = fields[3].parse::<u32>() else {
            continue;
        };
        if uid < minimum_uid || uid == u32::MAX {
            continue;
        }
        let user = UserRecord {
            name: fields[0].to_string(),
            uid,
            gid,
            gecos: fields[4].to_string(),
            home: fields[5].to_string(),
            shell: fields[6].to_string(),
        };
        if user.is_login_shell() {
            users.push(user);
        }
    }
    users.sort_by(|a, b| {
        a.display_name()
            .to_lowercase()
            .cmp(&b.display_name().to_lowercase())
    });
    users
}

pub fn authenticate(
    username: &str,
    password: &[u8],
    allow_root: bool,
    minimum_uid: u32,
) -> Result<UserRecord, AuthFailure> {
    let user = lookup_user(username).ok_or_else(AuthFailure::denied)?;
    if user.uid == 0 && !allow_root {
        // Verify anyway so a policy rejection does not become a timing oracle
        // for whether the root password is valid.
        let _ = verify_against_shadow(username, password);
        return Err(AuthFailure {
            error: AuthError::Policy,
            message: "该账户不允许通过图形界面登录",
        });
    }
    if user.uid != 0 && user.uid < minimum_uid {
        return Err(AuthFailure::denied());
    }
    if !user.is_login_shell() {
        return Err(AuthFailure {
            error: AuthError::Disabled,
            message: "该账户没有可登录的 Shell",
        });
    }
    let shadow = match lookup_shadow(username) {
        Some(shadow) => shadow,
        None => return Err(AuthFailure::denied()),
    };
    if shadow.hash.starts_with('!') || shadow.hash.starts_with('*') {
        return Err(AuthFailure::denied());
    }
    if !verify_password(password, &shadow.hash) {
        return Err(AuthFailure::denied());
    }

    let today = days_since_epoch();
    if shadow.expire >= 0 && today > shadow.expire {
        return Err(AuthFailure {
            error: AuthError::Expired,
            message: "账户已过期，请联系管理员",
        });
    }
    if shadow.last_change == 0 {
        return Err(AuthFailure {
            error: AuthError::MustChange,
            message: "密码已过期，请先在控制台修改密码",
        });
    }
    if shadow.inactive >= 0 && today > shadow.last_change.saturating_add(shadow.inactive) {
        return Err(AuthFailure {
            error: AuthError::Expired,
            message: "账户因长期未登录已失效",
        });
    }
    if shadow.maximum > 0 && today > shadow.last_change.saturating_add(shadow.maximum) {
        return Err(AuthFailure {
            error: AuthError::Expired,
            message: "密码已过期，请先在控制台修改密码",
        });
    }
    Ok(user)
}

/// Used for unknown accounts to keep the failed-login path roughly constant
/// time.  The daemon computes this once at startup.
pub fn dummy_verify(password: &[u8], hash: &str) {
    let _ = verify_password(password, hash);
}

pub fn verify_password(password: &[u8], hash: &str) -> bool {
    if hash.starts_with('!') || hash.starts_with('*') {
        return false;
    }
    if hash.is_empty() {
        return password.is_empty();
    }
    let Ok(password) = CString::new(password) else {
        return false;
    };
    let Ok(hash_c) = CString::new(hash) else {
        return false;
    };
    let result = unsafe { crypt(password.as_ptr(), hash_c.as_ptr()) };
    if result.is_null() {
        return false;
    }
    let result = unsafe { CStr::from_ptr(result) }.to_bytes();
    constant_time_eq(result, hash.as_bytes())
}

fn verify_against_shadow(username: &str, password: &[u8]) -> bool {
    lookup_shadow(username)
        .map(|shadow| verify_password(password, &shadow.hash))
        .unwrap_or(false)
}

pub fn hash_password(password: &[u8]) -> Result<String, String> {
    let salt = random_salt();
    let salt_c = CString::new(format!("$6${salt}$")).map_err(|_| "salt NUL".to_string())?;
    let password_c = CString::new(password).map_err(|_| "密码包含 NUL 字符".to_string())?;
    let result = unsafe { crypt(password_c.as_ptr(), salt_c.as_ptr()) };
    if result.is_null() {
        return Err("crypt(3) 失败".to_string());
    }
    let hash = unsafe { CStr::from_ptr(result) }
        .to_string_lossy()
        .into_owned();
    if !hash.starts_with("$6$") {
        return Err("系统 crypt 未生成 SHA-512 哈希".to_string());
    }
    Ok(hash)
}

fn random_salt() -> String {
    let mut bytes = [0u8; 16];
    if let Ok(mut random) = std::fs::File::open("/dev/urandom") {
        use std::io::Read;
        if random.read_exact(&mut bytes).is_err() {
            // This is unreachable on a working kernel; the fallback still
            // produces a unique salt rather than silently reusing one.
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos() as u64);
            bytes[..8].copy_from_slice(&now.to_le_bytes());
            bytes[8..].copy_from_slice(&std::process::id().to_le_bytes());
        }
    }
    bytes
        .iter()
        .map(|byte| CRYPT_ALPHABET[*byte as usize % CRYPT_ALPHABET.len()] as char)
        .collect()
}

pub fn days_since_epoch() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| (duration.as_secs() / 86_400) as i64)
}

const COMMON_PASSWORDS: &[&str] = &[
    "password",
    "password1",
    "password123",
    "12345678",
    "123456789",
    "1234567890",
    "qwertyui",
    "qwerty123",
    "qwertyuiop",
    "abc12345",
    "11111111",
    "00000000",
    "iloveyou",
    "admin123",
    "administrator",
    "letmein1",
    "welcome1",
    "welcome123",
    "monkey123",
    "dragon123",
    "football",
    "baseball",
    "sunshine",
    "princess",
    "trustno1",
    "passw0rd",
    "p@ssw0rd",
    "spaceos",
    "spacekernel",
    "changeme",
    "default",
    "test1234",
    "user1234",
    "linux123",
    "root1234",
    "master123",
    "shadow1",
    "superman",
    "batman123",
    "starwars",
];

/// Returns a 0..=4 strength score, or a human-readable rejection reason.
pub fn validate_password(
    password: &str,
    username: &str,
    full_name: &str,
) -> Result<u8, &'static str> {
    let bytes = password.as_bytes();
    if bytes.len() < 8 {
        return Err("密码至少需要 8 个字符");
    }
    if bytes.len() > 128 {
        return Err("密码不能超过 128 个字符");
    }
    if password.chars().any(|c| c.is_control()) {
        return Err("密码不能包含控制字符");
    }
    if password.trim().is_empty() {
        return Err("密码不能只包含空白字符");
    }
    let lower = password.to_lowercase();
    if COMMON_PASSWORDS.contains(&lower.as_str()) {
        return Err("该密码出现在常见密码列表中");
    }
    let username_lower = username.to_lowercase();
    if username_lower.len() >= 3 && lower.contains(&username_lower) {
        return Err("密码不能包含用户名");
    }
    let full_part = full_name
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_lowercase();
    if full_part.len() >= 3 && lower.contains(&full_part) {
        return Err("密码不能包含显示名称");
    }
    let mut classes = 0;
    if bytes.iter().any(u8::is_ascii_lowercase) {
        classes += 1;
    }
    if bytes.iter().any(u8::is_ascii_uppercase) {
        classes += 1;
    }
    if bytes.iter().any(u8::is_ascii_digit) {
        classes += 1;
    }
    if bytes
        .iter()
        .any(|byte| !byte.is_ascii_alphanumeric() && !byte.is_ascii_whitespace())
    {
        classes += 1;
    }
    if bytes.len() < 10 && classes < 2 {
        return Err("8 到 9 位密码至少需要包含两类字符");
    }
    if bytes.len() < 12 && classes < 2 {
        return Err("12 位以下的密码至少需要包含两类字符");
    }
    let unique = bytes
        .iter()
        .copied()
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    if unique * 3 < bytes.len() {
        return Err("密码重复字符过多");
    }
    if is_sequential(&lower) {
        return Err("密码不能是连续字符序列");
    }
    let score = match (bytes.len(), classes, unique) {
        (12.., 3.., 8..) => 4,
        (12.., 2.., _) => 3,
        (10.., 2.., _) => 2,
        _ => 1,
    };
    Ok(score)
}

pub fn password_strength_label(score: u8) -> &'static str {
    match score {
        0 => "太弱",
        1 => "较弱",
        2 => "一般",
        3 => "较强",
        _ => "很强",
    }
}

fn is_sequential(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() < 6 {
        return false;
    }
    let mut ascending = 0usize;
    let mut descending = 0usize;
    for pair in bytes.windows(2) {
        if pair[1] == pair[0].wrapping_add(1) {
            ascending += 1;
        }
        if pair[1].wrapping_add(1) == pair[0] {
            descending += 1;
        }
    }
    ascending + 2 >= bytes.len() || descending + 2 >= bytes.len()
}

/// Path-safe check for an account home directory.  Kept here because both the
/// OOBE account transaction and the greeter's session launcher use it.
pub fn valid_home(path: &str) -> bool {
    path.starts_with('/')
        && !path.contains("..")
        && !path.contains('\0')
        && Path::new(path).components().count() >= 2
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut difference = 0u8;
    for (left, right) in a.iter().zip(b.iter()) {
        difference |= left ^ right;
    }
    difference == 0
}

unsafe fn cstr(pointer: *const c_char) -> String {
    if pointer.is_null() {
        return String::new();
    }
    CStr::from_ptr(pointer).to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_policy_rejects_common_and_short() {
        assert!(validate_password("short", "alice", "Alice").is_err());
        assert!(validate_password("password123", "alice", "Alice").is_err());
        assert!(validate_password("alice12345", "alice", "Alice").is_err());
        assert!(validate_password("aaaaaaaaaaaa", "alice", "Alice").is_err());
        assert!(validate_password("abcdefghijkl", "alice", "Alice").is_err());
    }

    #[test]
    fn password_policy_accepts_strong_secret() {
        assert!(validate_password("SpaceOS-9!keeps-going", "alice", "Alice").is_ok());
        assert!(validate_password("Tr0ub4dor&3", "alice", "Alice").is_ok());
    }

    #[test]
    fn hash_round_trip_uses_sha512() {
        let hash = hash_password(b"correct horse battery staple").expect("hash");
        assert!(hash.starts_with("$6$"));
        assert!(verify_password(b"correct horse battery staple", &hash));
        assert!(!verify_password(b"wrong password", &hash));
    }

    #[test]
    fn rejects_locked_hashes() {
        assert!(!verify_password(b"anything", "!$6$salt$hash"));
        assert!(!verify_password(b"anything", "*"));
        assert!(!verify_password(b"anything", ""));
    }

    #[test]
    fn empty_password_round_trip() {
        let hash = hash_password(b"").expect("hash empty password");
        assert!(hash.starts_with("$6$"));
        assert!(verify_password(b"", &hash));
        assert!(!verify_password(b"not empty", &hash));
        assert!(verify_password(b"", ""));
    }

    #[test]
    fn sequential_detection() {
        assert!(is_sequential("123456789"));
        assert!(is_sequential("abcdefgh"));
        assert!(!is_sequential("1a2b3c4d"));
    }
}
