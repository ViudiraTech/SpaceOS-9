//
//       user/spk/spk/src/bin/spk_sandbox.rs
//       Private mount-namespace script sandbox
//
//       2026/11/22 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

use std::ffi::CString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

const MS_RDONLY: libc::c_ulong = 1;
const MS_NOSUID: libc::c_ulong = 2;
const MS_NODEV: libc::c_ulong = 4;
const MS_NOEXEC: libc::c_ulong = 8;
const MS_REC: libc::c_ulong = 16384;
const MS_PRIVATE: libc::c_ulong = 1 << 18;
const MS_BIND: libc::c_ulong = 4096;
const MNT_DETACH: libc::c_int = 2;

const CLONE_NEWNS: libc::c_int = 0x0002_0000;
const CLONE_NEW_MASK: u64 = 0x0002_0000
    | 0x0200_0000
    | 0x0400_0000
    | 0x0800_0000
    | 0x1000_0000
    | 0x2000_0000
    | 0x4000_0000
    | 0x80;

#[cfg(target_arch = "x86_64")]
const SYS_UNSHARE: libc::c_long = 272;
#[cfg(target_arch = "x86_64")]
const SYS_PIVOT_ROOT: libc::c_long = 155;
#[cfg(target_arch = "x86_64")]
const SYS_CLOSE_RANGE: libc::c_long = 436;
#[cfg(target_arch = "x86_64")]
const SYS_SECCOMP: libc::c_long = 317;
#[cfg(target_arch = "x86_64")]
const SYS_CLONE: libc::c_long = 56;
#[cfg(target_arch = "x86_64")]
const SYS_CLONE3: libc::c_long = 435;
#[cfg(target_arch = "x86_64")]
const SYS_SOCKET: libc::c_long = 41;
#[cfg(target_arch = "x86_64")]
const SYS_CONNECT: libc::c_long = 42;
#[cfg(target_arch = "x86_64")]
const SYS_ACCEPT: libc::c_long = 43;
#[cfg(target_arch = "x86_64")]
const SYS_SENDTO: libc::c_long = 44;
#[cfg(target_arch = "x86_64")]
const SYS_RECVFROM: libc::c_long = 45;
#[cfg(target_arch = "x86_64")]
const SYS_SENDMSG: libc::c_long = 46;
#[cfg(target_arch = "x86_64")]
const SYS_RECVMSG: libc::c_long = 47;
#[cfg(target_arch = "x86_64")]
const SYS_BIND: libc::c_long = 49;
#[cfg(target_arch = "x86_64")]
const SYS_LISTEN: libc::c_long = 50;
#[cfg(target_arch = "x86_64")]
const SYS_ACCEPT4: libc::c_long = 288;
#[cfg(target_arch = "x86_64")]
const SYS_SOCKETPAIR: libc::c_long = 53;
#[cfg(target_arch = "x86_64")]
const SYS_MOUNT: libc::c_long = 165;
#[cfg(target_arch = "x86_64")]
const SYS_UMOUNT2: libc::c_long = 166;
#[cfg(target_arch = "x86_64")]
const SYS_CHROOT: libc::c_long = 161;
#[cfg(target_arch = "x86_64")]
const SYS_SETNS: libc::c_long = 308;
#[cfg(target_arch = "x86_64")]
const SYS_MKNOD: libc::c_long = 133;
#[cfg(target_arch = "x86_64")]
const SYS_MKNODAT: libc::c_long = 259;
#[cfg(target_arch = "x86_64")]
const SYS_PTRACE: libc::c_long = 101;
#[cfg(target_arch = "x86_64")]
const SYS_BPF: libc::c_long = 321;
#[cfg(target_arch = "x86_64")]
const SYS_PERF_EVENT_OPEN: libc::c_long = 298;
#[cfg(target_arch = "x86_64")]
const SYS_USERFAULTFD: libc::c_long = 323;
#[cfg(target_arch = "x86_64")]
const SYS_IO_URING_SETUP: libc::c_long = 425;
#[cfg(target_arch = "x86_64")]
const SYS_KEYCTL: libc::c_long = 250;
#[cfg(target_arch = "x86_64")]
const SYS_ADD_KEY: libc::c_long = 248;
#[cfg(target_arch = "x86_64")]
const SYS_REQUEST_KEY: libc::c_long = 249;
#[cfg(target_arch = "x86_64")]
const SYS_PROCESS_VM_READV: libc::c_long = 310;
#[cfg(target_arch = "x86_64")]
const SYS_PROCESS_VM_WRITEV: libc::c_long = 311;
#[cfg(target_arch = "x86_64")]
const SYS_OPEN_BY_HANDLE_AT: libc::c_long = 304;
#[cfg(target_arch = "x86_64")]
const SYS_NAME_TO_HANDLE_AT: libc::c_long = 303;
#[cfg(target_arch = "x86_64")]
const SYS_REBOOT: libc::c_long = 169;
#[cfg(target_arch = "x86_64")]
const SYS_KEXEC_LOAD: libc::c_long = 246;
#[cfg(target_arch = "x86_64")]
const SYS_INIT_MODULE: libc::c_long = 175;
#[cfg(target_arch = "x86_64")]
const SYS_FINIT_MODULE: libc::c_long = 313;
#[cfg(target_arch = "x86_64")]
const SYS_DELETE_MODULE: libc::c_long = 176;
#[cfg(target_arch = "x86_64")]
const SYS_SWAPON: libc::c_long = 167;
#[cfg(target_arch = "x86_64")]
const SYS_SWAPOFF: libc::c_long = 168;
#[cfg(target_arch = "x86_64")]
const SYS_ACCT: libc::c_long = 163;

#[cfg(target_arch = "aarch64")]
const SYS_UNSHARE: libc::c_long = 97;
#[cfg(target_arch = "aarch64")]
const SYS_PIVOT_ROOT: libc::c_long = 41;
#[cfg(target_arch = "aarch64")]
const SYS_CLOSE_RANGE: libc::c_long = 436;
#[cfg(target_arch = "aarch64")]
const SYS_SECCOMP: libc::c_long = 277;
#[cfg(target_arch = "aarch64")]
const SYS_CLONE: libc::c_long = 220;
#[cfg(target_arch = "aarch64")]
const SYS_CLONE3: libc::c_long = 435;
#[cfg(target_arch = "aarch64")]
const SYS_SOCKET: libc::c_long = 198;
#[cfg(target_arch = "aarch64")]
const SYS_CONNECT: libc::c_long = 203;
#[cfg(target_arch = "aarch64")]
const SYS_ACCEPT: libc::c_long = 202;
#[cfg(target_arch = "aarch64")]
const SYS_SENDTO: libc::c_long = 206;
#[cfg(target_arch = "aarch64")]
const SYS_RECVFROM: libc::c_long = 207;
#[cfg(target_arch = "aarch64")]
const SYS_SENDMSG: libc::c_long = 211;
#[cfg(target_arch = "aarch64")]
const SYS_RECVMSG: libc::c_long = 212;
#[cfg(target_arch = "aarch64")]
const SYS_BIND: libc::c_long = 200;
#[cfg(target_arch = "aarch64")]
const SYS_LISTEN: libc::c_long = 201;
#[cfg(target_arch = "aarch64")]
const SYS_ACCEPT4: libc::c_long = 242;
#[cfg(target_arch = "aarch64")]
const SYS_SOCKETPAIR: libc::c_long = 199;
#[cfg(target_arch = "aarch64")]
const SYS_MOUNT: libc::c_long = 40;
#[cfg(target_arch = "aarch64")]
const SYS_UMOUNT2: libc::c_long = 39;
#[cfg(target_arch = "aarch64")]
const SYS_CHROOT: libc::c_long = 51;
#[cfg(target_arch = "aarch64")]
const SYS_SETNS: libc::c_long = 268;
#[cfg(target_arch = "aarch64")]
const SYS_MKNOD: libc::c_long = 33;
#[cfg(target_arch = "aarch64")]
const SYS_MKNODAT: libc::c_long = 33;
#[cfg(target_arch = "aarch64")]
const SYS_PTRACE: libc::c_long = 117;
#[cfg(target_arch = "aarch64")]
const SYS_BPF: libc::c_long = 280;
#[cfg(target_arch = "aarch64")]
const SYS_PERF_EVENT_OPEN: libc::c_long = 241;
#[cfg(target_arch = "aarch64")]
const SYS_USERFAULTFD: libc::c_long = 282;
#[cfg(target_arch = "aarch64")]
const SYS_IO_URING_SETUP: libc::c_long = 425;
#[cfg(target_arch = "aarch64")]
const SYS_KEYCTL: libc::c_long = 219;
#[cfg(target_arch = "aarch64")]
const SYS_ADD_KEY: libc::c_long = 217;
#[cfg(target_arch = "aarch64")]
const SYS_REQUEST_KEY: libc::c_long = 218;
#[cfg(target_arch = "aarch64")]
const SYS_PROCESS_VM_READV: libc::c_long = 270;
#[cfg(target_arch = "aarch64")]
const SYS_PROCESS_VM_WRITEV: libc::c_long = 271;
#[cfg(target_arch = "aarch64")]
const SYS_OPEN_BY_HANDLE_AT: libc::c_long = 265;
#[cfg(target_arch = "aarch64")]
const SYS_NAME_TO_HANDLE_AT: libc::c_long = 264;
#[cfg(target_arch = "aarch64")]
const SYS_REBOOT: libc::c_long = 142;
#[cfg(target_arch = "aarch64")]
const SYS_KEXEC_LOAD: libc::c_long = 104;
#[cfg(target_arch = "aarch64")]
const SYS_INIT_MODULE: libc::c_long = 105;
#[cfg(target_arch = "aarch64")]
const SYS_FINIT_MODULE: libc::c_long = 273;
#[cfg(target_arch = "aarch64")]
const SYS_DELETE_MODULE: libc::c_long = 106;
#[cfg(target_arch = "aarch64")]
const SYS_SWAPON: libc::c_long = 224;
#[cfg(target_arch = "aarch64")]
const SYS_SWAPOFF: libc::c_long = 225;
#[cfg(target_arch = "aarch64")]
const SYS_ACCT: libc::c_long = 89;

// riscv64 uses the asm-generic syscall table, same as aarch64 for these calls.
#[cfg(target_arch = "riscv64")]
use aarch64_numbers::*;
#[cfg(target_arch = "riscv64")]
mod aarch64_numbers {
    pub const SYS_UNSHARE: libc::c_long = 97;
    pub const SYS_PIVOT_ROOT: libc::c_long = 41;
    pub const SYS_CLOSE_RANGE: libc::c_long = 436;
    pub const SYS_SECCOMP: libc::c_long = 277;
    pub const SYS_CLONE: libc::c_long = 220;
    pub const SYS_CLONE3: libc::c_long = 435;
    pub const SYS_SOCKET: libc::c_long = 198;
    pub const SYS_CONNECT: libc::c_long = 203;
    pub const SYS_ACCEPT: libc::c_long = 202;
    pub const SYS_SENDTO: libc::c_long = 206;
    pub const SYS_RECVFROM: libc::c_long = 207;
    pub const SYS_SENDMSG: libc::c_long = 211;
    pub const SYS_RECVMSG: libc::c_long = 212;
    pub const SYS_BIND: libc::c_long = 200;
    pub const SYS_LISTEN: libc::c_long = 201;
    pub const SYS_ACCEPT4: libc::c_long = 242;
    pub const SYS_SOCKETPAIR: libc::c_long = 199;
    pub const SYS_MOUNT: libc::c_long = 40;
    pub const SYS_UMOUNT2: libc::c_long = 39;
    pub const SYS_CHROOT: libc::c_long = 51;
    pub const SYS_SETNS: libc::c_long = 268;
    pub const SYS_MKNOD: libc::c_long = 33;
    pub const SYS_MKNODAT: libc::c_long = 33;
    pub const SYS_PTRACE: libc::c_long = 117;
    pub const SYS_BPF: libc::c_long = 280;
    pub const SYS_PERF_EVENT_OPEN: libc::c_long = 241;
    pub const SYS_USERFAULTFD: libc::c_long = 282;
    pub const SYS_IO_URING_SETUP: libc::c_long = 425;
    pub const SYS_KEYCTL: libc::c_long = 219;
    pub const SYS_ADD_KEY: libc::c_long = 217;
    pub const SYS_REQUEST_KEY: libc::c_long = 218;
    pub const SYS_PROCESS_VM_READV: libc::c_long = 270;
    pub const SYS_PROCESS_VM_WRITEV: libc::c_long = 271;
    pub const SYS_OPEN_BY_HANDLE_AT: libc::c_long = 265;
    pub const SYS_NAME_TO_HANDLE_AT: libc::c_long = 264;
    pub const SYS_REBOOT: libc::c_long = 142;
    pub const SYS_KEXEC_LOAD: libc::c_long = 104;
    pub const SYS_INIT_MODULE: libc::c_long = 105;
    pub const SYS_FINIT_MODULE: libc::c_long = 273;
    pub const SYS_DELETE_MODULE: libc::c_long = 106;
    pub const SYS_SWAPON: libc::c_long = 224;
    pub const SYS_SWAPOFF: libc::c_long = 225;
    pub const SYS_ACCT: libc::c_long = 89;
}

const BPF_LD: u16 = 0x00;
const BPF_ALU: u16 = 0x04;
const BPF_JMP: u16 = 0x05;
const BPF_RET: u16 = 0x06;
const BPF_W: u16 = 0x00;
const BPF_ABS: u16 = 0x20;
const BPF_AND: u16 = 0x50;
const BPF_JEQ: u16 = 0x10;
const BPF_K: u16 = 0x00;
const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;
const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
const SECCOMP_SET_MODE_FILTER: libc::c_uint = 1;

#[repr(C)]
struct SockFilter {
    code: u16,
    jt: u8,
    jf: u8,
    k: u32,
}

#[repr(C)]
struct SockFprog {
    len: u16,
    filter: *const SockFilter,
}

#[derive(Default)]
struct Options {
    root: PathBuf,
    stage: PathBuf,
    source: PathBuf,
    scripts: PathBuf,
    uid: u32,
    gid: u32,
    program: String,
    args: Vec<String>,
}

fn cstring(value: &str) -> CString {
    CString::new(value.as_bytes()).expect("sandbox path contains NUL")
}

fn fail(message: impl std::fmt::Display) -> ! {
    eprintln!("spk-sandbox: {message}");
    std::process::exit(125);
}

fn check(rc: libc::c_int, what: &str) {
    if rc != 0 {
        fail(format!("{what}: {}", std::io::Error::last_os_error()));
    }
}

fn syscall(rc: libc::c_long, what: &str) -> libc::c_long {
    if rc < 0 {
        fail(format!("{what}: {}", std::io::Error::last_os_error()));
    }
    rc
}

fn mount(source: &str, target: &Path, fstype: &str, flags: libc::c_ulong, data: &str) {
    let source = cstring(source);
    let target = cstring(target.to_string_lossy().as_ref());
    let fstype = cstring(fstype);
    let data = cstring(data);
    let rc = unsafe {
        libc::mount(
            source.as_ptr(),
            target.as_ptr(),
            fstype.as_ptr(),
            flags,
            data.as_ptr() as *const libc::c_void,
        )
    };
    if rc != 0 {
        fail(format!(
            "mount {source:?} on {target:?}: {}",
            std::io::Error::last_os_error()
        ));
    }
}

fn bind(source: &Path, target: &Path, flags: libc::c_ulong) {
    mount(
        source.to_string_lossy().as_ref(),
        target,
        "none",
        MS_BIND | flags,
        "",
    );
}

fn mkdir(path: &Path) {
    fs::create_dir_all(path)
        .unwrap_or_else(|error| fail(format!("mkdir {}: {error}", path.display())));
}

fn parse_options() -> Options {
    let mut options = Options::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--root" => options.root = PathBuf::from(args.next().unwrap_or_default()),
            "--stage" => options.stage = PathBuf::from(args.next().unwrap_or_default()),
            "--source" => options.source = PathBuf::from(args.next().unwrap_or_default()),
            "--scripts" => options.scripts = PathBuf::from(args.next().unwrap_or_default()),
            "--uid" => options.uid = args.next().unwrap_or_default().parse().unwrap_or(0),
            "--gid" => options.gid = args.next().unwrap_or_default().parse().unwrap_or(0),
            "--" => {
                options.program = args.next().unwrap_or_default();
                options.args = args.collect();
                break;
            }
            other => fail(format!("unknown argument {other}")),
        }
    }
    if options.root.as_os_str().is_empty()
        || options.stage.as_os_str().is_empty()
        || options.source.as_os_str().is_empty()
        || options.scripts.as_os_str().is_empty()
        || options.program.is_empty()
        || options.uid == 0
        || options.gid == 0
    {
        fail("missing --root/--stage/--source/--scripts/--uid/--gid/-- program");
    }
    options
}

fn setup(options: &Options) {
    if unsafe { libc::geteuid() } != 0 {
        fail("spk-sandbox must be started by root");
    }
    syscall(
        unsafe { libc::syscall(SYS_UNSHARE, CLONE_NEWNS) },
        "unshare(CLONE_NEWNS)",
    );
    // The current root is the source root; make propagation private.
    mount("none", Path::new("/"), "none", MS_REC | MS_PRIVATE, "");
    mkdir(&options.root);
    mount("none", &options.root, "ramfs", MS_NOSUID | MS_NODEV, "");
    let root = &options.root;
    for directory in [
        "stage",
        "source",
        "bin",
        "lib",
        "lib64",
        "usr",
        "etc",
        "dev",
        "proc",
        "tmp",
        "spk-scripts",
        "oldroot",
    ] {
        mkdir(&root.join(directory));
    }
    bind(&options.stage, &root.join("stage"), 0);
    bind(
        &options.source,
        &root.join("source"),
        MS_RDONLY | MS_NOSUID | MS_NODEV,
    );
    for directory in ["bin", "lib", "lib64", "usr", "etc"] {
        let source = options.source.join(directory);
        if source.exists() {
            let extra = if directory == "etc" { MS_NOEXEC } else { 0 };
            bind(
                &source,
                &root.join(directory),
                MS_RDONLY | MS_NOSUID | MS_NODEV | extra,
            );
        }
    }
    // Scriptlets always get BusyBox ash at /bin/sh; the native SpaceOS shell
    // intentionally has no function/source support.
    let busybox = options.source.join("bin/busybox");
    if busybox.is_file() && root.join("bin/sh").exists() {
        bind(
            &busybox,
            &root.join("bin/sh"),
            MS_RDONLY | MS_NOSUID | MS_NODEV,
        );
    }
    bind(
        &options.scripts,
        &root.join("spk-scripts"),
        MS_RDONLY | MS_NOSUID | MS_NODEV | MS_NOEXEC,
    );
    mount(
        "none",
        &root.join("dev"),
        "devfs",
        MS_NOSUID | MS_NOEXEC,
        "",
    );
    mount(
        "none",
        &root.join("proc"),
        "proc",
        MS_NOSUID | MS_NODEV | MS_NOEXEC,
        "hidepid=2",
    );
    let tmp = root.join("tmp");
    let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o700));
    unsafe {
        libc::chown(
            cstring(&tmp.to_string_lossy()).as_ptr(),
            options.uid,
            options.gid,
        );
    }
    // pivot_root is the primary path; chroot is a compatibility fallback.
    let pivot = unsafe {
        libc::syscall(
            SYS_PIVOT_ROOT,
            cstring(&root.to_string_lossy()).as_ptr(),
            cstring(&root.join("oldroot").to_string_lossy()).as_ptr(),
        )
    };
    if pivot == 0 {
        check(unsafe { libc::chdir(cstring("/").as_ptr()) }, "chdir /");
        let _ = unsafe { libc::umount2(cstring("/oldroot").as_ptr(), MNT_DETACH) };
    } else {
        check(
            unsafe { libc::chroot(cstring(&root.to_string_lossy()).as_ptr()) },
            "chroot fallback",
        );
        check(unsafe { libc::chdir(cstring("/").as_ptr()) }, "chdir /");
    }
}

fn close_extra_fds() {
    let rc = unsafe { libc::syscall(SYS_CLOSE_RANGE, 3u32, u32::MAX, 0u32) };
    if rc != 0 {
        for fd in 3..1024 {
            unsafe {
                libc::close(fd);
            }
        }
    }
}

#[cfg(target_env = "musl")]
type RlimitResource = libc::c_int;
#[cfg(not(target_env = "musl"))]
type RlimitResource = u32;

fn limits() {
    let set = |resource: RlimitResource, value: u64| {
        let limit = libc::rlimit {
            rlim_cur: value as libc::rlim_t,
            rlim_max: value as libc::rlim_t,
        };
        unsafe {
            libc::setrlimit(resource, &limit);
        }
    };
    set(libc::RLIMIT_CPU as RlimitResource, 60);
    set(libc::RLIMIT_AS as RlimitResource, 512 * 1024 * 1024);
    set(libc::RLIMIT_FSIZE as RlimitResource, 64 * 1024 * 1024);
    set(libc::RLIMIT_NOFILE as RlimitResource, 64);
    set(libc::RLIMIT_NPROC as RlimitResource, 64);
    set(libc::RLIMIT_CORE as RlimitResource, 0);
}

fn install_seccomp() {
    let mut program = Vec::new();
    // A = syscall number.
    program.push(SockFilter {
        code: BPF_LD | BPF_W | BPF_ABS,
        jt: 0,
        jf: 0,
        k: 0,
    });
    let denied = [
        SYS_SOCKET,
        SYS_CONNECT,
        SYS_ACCEPT,
        SYS_SENDTO,
        SYS_RECVFROM,
        SYS_SENDMSG,
        SYS_RECVMSG,
        SYS_BIND,
        SYS_LISTEN,
        SYS_ACCEPT4,
        SYS_SOCKETPAIR,
        SYS_MOUNT,
        SYS_UMOUNT2,
        SYS_PIVOT_ROOT,
        SYS_CHROOT,
        SYS_SETNS,
        SYS_UNSHARE,
        SYS_MKNOD,
        SYS_MKNODAT,
        SYS_PTRACE,
        SYS_BPF,
        SYS_PERF_EVENT_OPEN,
        SYS_USERFAULTFD,
        SYS_IO_URING_SETUP,
        SYS_KEYCTL,
        SYS_ADD_KEY,
        SYS_REQUEST_KEY,
        SYS_PROCESS_VM_READV,
        SYS_PROCESS_VM_WRITEV,
        SYS_OPEN_BY_HANDLE_AT,
        SYS_NAME_TO_HANDLE_AT,
        SYS_REBOOT,
        SYS_KEXEC_LOAD,
        SYS_INIT_MODULE,
        SYS_FINIT_MODULE,
        SYS_DELETE_MODULE,
        SYS_SWAPON,
        SYS_SWAPOFF,
        SYS_ACCT,
        SYS_CLONE3,
    ];
    for number in denied {
        program.push(SockFilter {
            code: BPF_JMP | BPF_JEQ | BPF_K,
            jt: 0,
            jf: 1,
            k: number as u32,
        });
        program.push(SockFilter {
            code: BPF_RET | BPF_K,
            jt: 0,
            jf: 0,
            k: SECCOMP_RET_ERRNO | libc::EPERM as u32,
        });
    }
    // clone(2) with any CLONE_NEW* flag is denied. clone3 is denied entirely
    // above because its flags live behind a user pointer that cBPF cannot read.
    program.push(SockFilter {
        code: BPF_JMP | BPF_JEQ | BPF_K,
        jt: 0,
        jf: 4,
        k: SYS_CLONE as u32,
    });
    program.push(SockFilter {
        code: BPF_LD | BPF_W | BPF_ABS,
        jt: 0,
        jf: 0,
        k: 16,
    });
    program.push(SockFilter {
        code: BPF_ALU | BPF_AND | BPF_K,
        jt: 0,
        jf: 0,
        k: CLONE_NEW_MASK as u32,
    });
    program.push(SockFilter {
        code: BPF_JMP | BPF_JEQ | BPF_K,
        jt: 1,
        jf: 0,
        k: 0,
    });
    program.push(SockFilter {
        code: BPF_RET | BPF_K,
        jt: 0,
        jf: 0,
        k: SECCOMP_RET_ERRNO | libc::EPERM as u32,
    });
    program.push(SockFilter {
        code: BPF_RET | BPF_K,
        jt: 0,
        jf: 0,
        k: SECCOMP_RET_ALLOW,
    });
    let fprog = SockFprog {
        len: program.len() as u16,
        filter: program.as_ptr(),
    };
    let rc = unsafe {
        libc::syscall(
            SYS_SECCOMP,
            SECCOMP_SET_MODE_FILTER,
            0,
            &fprog as *const SockFprog,
        )
    };
    syscall(rc, "seccomp(SECCOMP_SET_MODE_FILTER)");
}

fn drop_privileges(options: &Options) {
    check(
        unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) },
        "PR_SET_NO_NEW_PRIVS",
    );
    check(
        unsafe { libc::setgroups(0, std::ptr::null()) },
        "setgroups(0)",
    );
    check(unsafe { libc::setgid(options.gid) }, "setgid");
    check(unsafe { libc::setuid(options.uid) }, "setuid");
    if unsafe { libc::geteuid() } == 0 || unsafe { libc::getegid() } == 0 {
        fail("privilege drop did not take effect");
    }
}

fn main() {
    let options = parse_options();
    setup(&options);
    close_extra_fds();
    unsafe {
        libc::setsid();
        libc::setpgid(0, 0);
    }
    limits();
    drop_privileges(&options);
    install_seccomp();
    let mut command = Command::new(&options.program);
    command
        .args(&options.args)
        .env_clear()
        .env(
            "PATH",
            "/stage/bin:/stage/usr/bin:/bin:/usr/bin:/source/bin:/source/usr/bin",
        )
        .env("HOME", "/stage/tmp")
        .env("TMPDIR", "/tmp")
        .env("SPK_ROOT", "/stage")
        .env("SPK_SOURCE", "/source")
        .env("SPK_SANDBOX", "1")
        .env("LANG", "C")
        .env("TERM", "dumb");
    let error = command.exec();
    fail(format!("exec {}: {error}", options.program));
}
