/*
 *
 *       src/user/exec.rs
 *       Script (#!) interpreter resolution for execve and init
 *
 *       2026/11/11 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 */

//! Linux-compatible shebang handling.  A script is replaced by its
//! interpreter, and argv becomes:
//!
//! `[interpreter, optional-argument?, script-path, original argv[1..]]`
//!
//! Interpreters may themselves be scripts, up to four levels as Linux does
//! with `BINPRM_MAX_RECURSION`.

use alloc::{
    string::{String, ToString},
    vec::Vec,
};

use crate::vfs::{self, VfsError};

const MAX_SHEBANG_DEPTH: u32 = 4;
const MAX_SHEBANG_LINE: usize = 256;

pub struct ResolvedExec {
    pub data: Vec<u8>,
    pub argv: Vec<String>,
    /// Script path as supplied to execve, used for comm and AT_EXECFN.
    pub path: String,
}

fn check_executable(path: &str) -> Result<(), VfsError> {
    let (inode, _) = vfs::lookup_abs(path, true)?;
    let meta = inode.metadata()?;
    if meta.kind != vfs::InodeKind::Regular || meta.mode & 0o111 == 0 {
        return Err(VfsError::PermissionDenied);
    }
    Ok(())
}

fn shebang_fields(data: &[u8]) -> Option<(String, Option<String>)> {
    if data.len() < 2 || data[0] != b'#' || data[1] != b'!' {
        return None;
    }
    let limit = data.len().min(MAX_SHEBANG_LINE);
    let mut end = 2;
    while end < limit && data[end] != b'\n' {
        end += 1;
    }
    let mut line = &data[2..end];
    while line.last() == Some(&b'\r') {
        line = &line[..line.len() - 1];
    }
    let mut index = 0;
    while index < line.len() && matches!(line[index], b' ' | b'\t') {
        index += 1;
    }
    let start = index;
    while index < line.len() && !matches!(line[index], b' ' | b'\t') {
        index += 1;
    }
    if index == start {
        return None;
    }
    let interpreter = String::from_utf8_lossy(&line[start..index]).into_owned();
    while index < line.len() && matches!(line[index], b' ' | b'\t') {
        index += 1;
    }
    let mut argument_end = line.len();
    while argument_end > index && matches!(line[argument_end - 1], b' ' | b'\t') {
        argument_end -= 1;
    }
    let argument = if argument_end > index {
        Some(String::from_utf8_lossy(&line[index..argument_end]).into_owned())
    } else {
        None
    };
    Some((interpreter, argument))
}

fn resolve_inner(path: &str, argv: &[String], depth: u32) -> Result<ResolvedExec, VfsError> {
    check_executable(path)?;
    let data = vfs::read_file(path)?;
    let Some((interpreter, argument)) = shebang_fields(&data) else {
        return Ok(ResolvedExec {
            data,
            argv: argv.to_vec(),
            path: path.to_string(),
        });
    };
    if depth == 0 {
        return Err(VfsError::ExecFormat);
    }
    let mut next_argv = Vec::with_capacity(argv.len() + 2);
    next_argv.push(interpreter.clone());
    if let Some(argument) = argument {
        next_argv.push(argument);
    }
    next_argv.push(path.to_string());
    next_argv.extend(argv.iter().skip(1).cloned());
    let mut resolved = resolve_inner(&interpreter, &next_argv, depth - 1)?;
    // Keep the script path for comm/AT_EXECFN even when the interpreter is a
    // chain of scripts.
    resolved.path = path.to_string();
    Ok(resolved)
}

/// Resolve `path` to the ELF image that actually runs.  `argv[0]` is replaced
/// by the interpreter for scripts, exactly as Linux does.
pub fn resolve(path: &str, argv: &[String]) -> Result<ResolvedExec, VfsError> {
    let fallback;
    let argv = if argv.is_empty() {
        fallback = [path.to_string()];
        &fallback[..]
    } else {
        argv
    };
    resolve_inner(path, argv, MAX_SHEBANG_DEPTH)
}
