/*
 *       src/vfs/procfs.rs
 *       Identity-bound process views and per-open telemetry snapshots
 *       2026/10/24 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 */
use super::{
    DirEntry, FileSystem, Inode, InodeKind, Stat, StatFs, VfsError,
    tree::{self, Snapshot},
};
use crate::{
    block, boot, cpuid, mm,
    sched::{self, TaskState, TaskWeak},
    time,
};
use alloc::{format, string::String, sync::Arc, vec::Vec};
const SYSTEM: &[&str] = &[
    "cpuinfo",
    "meminfo",
    "uptime",
    "version",
    "mounts",
    "filesystems",
    "partitions",
    "diskstats",
    "cmdline",
    "stat",
    "loadavg",
    #[cfg(feature = "kallsyms")]
    "kallsyms",
    #[cfg(feature = "modules")]
    "modules",
    #[cfg(feature = "sound")]
    "asound",
];
const PROCESS: &[&str] = &[
    "status", "stat", "statm", "syscall", "comm", "cmdline", "environ", "maps", "auxv", "cwd",
    "exe", "root", "fd", "fdinfo", "task", "mounts",
];
const SYS_FS: &[&str] = &["dentry-state", "inode-nr", "inode-state"];
enum View {
    Root,
    Sys,
    SysFs,
    SysFsFile(String),
    Process(TaskWeak),
    Tasks(TaskWeak),
    Fds(TaskWeak, bool),
    File(Option<TaskWeak>, String),
    Link(Option<TaskWeak>, String),
}
struct Node {
    id: u64,
    view: View,
}
impl Node {
    fn make(view: View, id: u64) -> Arc<dyn Inode> {
        Arc::new(Self { id, view })
    }
    fn task(w: &TaskWeak) -> Result<crate::sched::TaskRef, VfsError> {
        w.upgrade().ok_or(VfsError::NotFound)
    }
    fn bytes(&self) -> Result<Vec<u8>, VfsError> {
        sched::sync_accounting();
        match &self.view {
            View::File(Some(w), name) => {
                let task = Self::task(w)?;
                let stats = task.stats();
                let files = task.files();
                let space = task.address_space();
                // Filesystem metadata may sleep, so collect it before locking.
                let map_entries = match space.as_ref() {
                    Some(space) if name == "maps" => {
                        crate::user::UserAddressSpace::map_entries(space)
                    }
                    _ => alloc::vec::Vec::new(),
                };
                let space = space.as_ref().map(|s| s.lock());
                let image = space.as_ref().and_then(|s| s.image.as_ref());
                let comm_len = stats
                    .comm
                    .iter()
                    .position(|&byte| byte == 0)
                    .unwrap_or(stats.comm.len());
                let comm = String::from_utf8_lossy(&stats.comm[..comm_len]).into_owned();
                let taskname = if comm.is_empty() {
                    String::from(
                        image
                            .map(|i| i.executable.rsplit('/').next().unwrap_or("process"))
                            .unwrap_or("kthread"),
                    )
                } else {
                    comm
                };
                let state = match stats.state {
                    TaskState::Running | TaskState::Ready => "R",
                    TaskState::Blocked => "S",
                    TaskState::Zombie => "Z",
                    TaskState::Dead => "Z",
                };
                let pages = space.as_ref().map(|s| s.resident_pages()).unwrap_or(0);
                let text = match name.as_str() {
                    "cmdline" => return Ok(image.map(|i| i.argv.clone()).unwrap_or_default()),
                    "environ" => return Ok(image.map(|i| i.environ.clone()).unwrap_or_default()),
                    "auxv" => return Ok(image.map(|i| i.auxv.clone()).unwrap_or_default()),
                    "comm" => format!("{}\n", taskname),
                    "syscall" => match crate::syscall::current_syscall(stats.tid) {
                        Some((nr, a0, a1)) => format!("{nr} {a0:#x} {a1:#x}\n"),
                        None => String::from("running\n"),
                    },
                    "status" => format!(
                        "Name:\t{}\nState:\t{}\nTgid:\t{}\nPid:\t{}\nPPid:\t{}\nUid:\t0\t0\t0\t0\nGid:\t0\t0\t0\t0\nThreads:\t1\nVmSize:\t{} kB\nVmRSS:\t{} kB\nvoluntary_ctxt_switches:\t{}\nnonvoluntary_ctxt_switches:\t{}\n",
                        taskname,
                        state,
                        stats.tgid,
                        stats.tid,
                        stats.ppid,
                        pages * 4,
                        pages * 4,
                        stats.voluntary_switches,
                        stats.involuntary_switches
                    ),
                    "statm" => format!("{} {} 0 0 0 0 0\n", pages, pages),
                    "stat" => format!(
                        "{} ({}) {} {} {} {} 0 -1 0 0 0 0 0 {} {} 0 0 {} {} 1 0 {} {} {} 0 0 0 0 0 0 0 0 0 0 0 0 0 0 {} 0 0 0 0 0 0 0 0 0 0 {}\n",
                        stats.tid,
                        taskname.replace(')', "_"),
                        state,
                        stats.ppid,
                        stats.tgid,
                        stats.tgid,
                        stats.user_ns / 10_000_000,
                        stats.system_ns / 10_000_000,
                        20 + stats.nice,
                        stats.nice,
                        stats.start_ns / 10_000_000,
                        pages * 4096,
                        pages,
                        stats.cpu,
                        stats.exit_code
                    ),
                    "maps" => {
                        let mut out = String::new();
                        if let Some(s) = space.as_ref() {
                            let stack_top = crate::user::stack::stack_top();
                            let stack_base =
                                stack_top - crate::user::stack::USER_STACK_PAGES as u64 * 4096;
                            let brk_end =
                                crate::user::address_space::align_up(s.brk).unwrap_or(s.brk);
                            let exe_name = s.image.as_ref().map(|i| i.executable.as_str());
                            for (start, end, p, shared, rdev, ino) in map_entries {
                                let label = if s.brk > s.brk_base
                                    && start >= s.brk_base
                                    && end <= brk_end
                                {
                                    Some("[heap]")
                                } else if start >= stack_base && end <= stack_top {
                                    Some("[stack]")
                                } else if ino == 0 && start < s.brk_base {
                                    exe_name
                                } else {
                                    None
                                };
                                if let Some(lbl) = label {
                                    out.push_str(&format!(
                                        "{:08x}-{:08x} {}{}{}{} 00000000 {:02x}:{:02x} {} {}\n",
                                        start,
                                        end,
                                        if p & 1 != 0 { "r" } else { "-" },
                                        if p & 2 != 0 { "w" } else { "-" },
                                        if p & 4 != 0 { "x" } else { "-" },
                                        if shared { "s" } else { "p" },
                                        rdev >> 8,
                                        rdev & 0xff,
                                        ino,
                                        lbl
                                    ));
                                } else {
                                    out.push_str(&format!(
                                        "{:08x}-{:08x} {}{}{}{} 00000000 {:02x}:{:02x} {}\n",
                                        start,
                                        end,
                                        if p & 1 != 0 { "r" } else { "-" },
                                        if p & 2 != 0 { "w" } else { "-" },
                                        if p & 4 != 0 { "x" } else { "-" },
                                        if shared { "s" } else { "p" },
                                        rdev >> 8,
                                        rdev & 0xff,
                                        ino
                                    ));
                                }
                            }
                        }
                        out
                    }
                    "mounts" => mounts(),
                    _ if name.starts_with("fdinfo/") => {
                        let fd = name[7..].parse::<usize>().map_err(|_| VfsError::NotFound)?;
                        let entry = files.lock().get(fd).ok_or(VfsError::NotFound)?;
                        format!(
                            "pos:\t{}\nflags:\t0{:o}\nmnt_id:\t0\nino:\t{}\n",
                            entry.file.offset(),
                            entry.file.flags()
                                | if entry.cloexec {
                                    super::abi::O_CLOEXEC
                                } else {
                                    0
                                },
                            entry.file.inode.metadata()?.ino
                        )
                    }
                    _ => return Err(VfsError::NotFound),
                };
                Ok(text.into_bytes())
            }
            View::File(None, name) => Ok(system(name)?.into_bytes()),
            View::SysFsFile(name) => Ok(sys_fs(name)?.into_bytes()),
            _ => Err(VfsError::IsADirectory),
        }
    }
}
fn mounts() -> String {
    let mut out = String::new();
    for m in super::list_mounts() {
        out.push_str(&format!(
            "{} {} {} rw 0 0\n",
            m.device, m.mount_point, m.fs_type
        ));
    }
    out
}
fn system(name: &str) -> Result<String, VfsError> {
    Ok(match name {
        "version" => crate::spk::version_line(),
        "cmdline" => format!("{}\n", boot::cmdline()),
        "mounts" => mounts(),
        "filesystems" => String::from(
            "nodev\tramfs\nnodev\ttmpfs\nnodev\tdevtmpfs\nnodev\tproc\nnodev\tsysfs\n\tvfat\n",
        ),
        "meminfo" => {
            let s = mm::pmm::stats();
            format!(
                "MemTotal: {} kB\nMemFree: {} kB\nMemAvailable: {} kB\n",
                s.frames * 4,
                s.free * 4,
                s.free * 4
            )
        }
        "uptime" => {
            let t = time::uptime_micros();
            let idle: u64 = (0..boot::cpu_count())
                .map(|c| sched::cpu_time(c)[2] / 1000)
                .sum();
            format!(
                "{}.{:02} {}.{:02}\n",
                t / 1_000_000,
                t / 10_000 % 100,
                idle / 1_000_000,
                idle / 10_000 % 100
            )
        }
        "cpuinfo" => {
            let mut out = String::new();
            if let Some(info) = cpuid::info() {
                for cpu in 0..boot::cpu_count() {
                    out.push_str(&format!(
                        "processor\t: {}\nvendor_id\t: {}\nmodel name\t: {}\n\n",
                        cpu,
                        info.vendor.as_str(),
                        info.model.as_str()
                    ));
                }
            }
            out
        }
        "partitions" => {
            let mut out = String::from("major minor  #blocks  name\n\n");
            for (id, dev) in block::devices().iter().enumerate() {
                let info = dev.info();
                out.push_str(&format!(
                    "8 {} {} {}\n",
                    id,
                    info.capacity_bytes() / 1024,
                    info.name
                ));
            }
            out
        }
        "diskstats" => {
            let mut out = String::new();
            for (id, dev) in block::devices().iter().enumerate() {
                let s = block::accounting::stats(id);
                out.push_str(&format!(
                    "8 {} {} {} 0 {} {} {} 0 {} {} {} {} {}\n",
                    id,
                    dev.info().name,
                    s.reads,
                    s.read_sectors,
                    s.read_us / 1000,
                    s.writes,
                    s.write_sectors,
                    s.write_us / 1000,
                    s.inflight,
                    s.busy_us / 1000,
                    s.weighted_us / 1000
                ));
            }
            out
        }
        "stat" => {
            let mut totals = [0; 3];
            let mut cpus = String::new();
            for c in 0..boot::cpu_count() {
                let t = sched::cpu_time(c);
                for i in 0..3 {
                    totals[i] += t[i];
                }
                cpus.push_str(&format!(
                    "cpu{} {} 0 {} {} 0 0 0 0 0 0\n",
                    c,
                    t[0] / 10_000_000,
                    t[1] / 10_000_000,
                    t[2] / 10_000_000
                ));
            }
            format!(
                "cpu {} 0 {} {} 0 0 0 0 0 0\n{}ctxt {}\nprocesses {}\nprocs_running {}\nprocs_blocked {}\n",
                totals[0] / 10_000_000,
                totals[1] / 10_000_000,
                totals[2] / 10_000_000,
                cpus,
                sched::context_switches(),
                sched::task_list().len() as u64 + sched::completed_count(),
                sched::task_list()
                    .iter()
                    .filter(|t| matches!(t.state, TaskState::Ready | TaskState::Running))
                    .count(),
                sched::task_list()
                    .iter()
                    .filter(|t| t.state == TaskState::Blocked)
                    .count()
            )
        }
        "loadavg" => {
            let tasks = sched::task_list();
            let avg = sched::loadavg();
            format!(
                "{}.{:02} {}.{:02} {}.{:02} {}/{} {}\n",
                avg[0].0,
                avg[0].1,
                avg[1].0,
                avg[1].1,
                avg[2].0,
                avg[2].1,
                tasks
                    .iter()
                    .filter(|t| matches!(t.state, TaskState::Running | TaskState::Ready))
                    .count(),
                tasks.len(),
                sched::last_tid()
            )
        }
        #[cfg(feature = "kallsyms")]
        "kallsyms" => {
            let mut out = String::new();
            crate::kallsyms::append(&mut out);
            #[cfg(feature = "modules")]
            crate::module::append_kallsyms(&mut out);
            out
        }
        #[cfg(feature = "modules")]
        "modules" => crate::module::proc_modules(),
        _ => return Err(VfsError::NotFound),
    })
}
fn sys_fs(name: &str) -> Result<String, VfsError> {
    let stats = super::dcache::stats();
    Ok(match name {
        // Linux /proc/sys/fs/dentry-state:
        // nr_dentry nr_unused age_limit want_pages nr_negative
        "dentry-state" => format!(
            "{} {} 0 0 {}\n",
            stats.dentries, stats.unused, stats.negative
        ),
        // Linux /proc/sys/fs/inode-nr: nr_inodes nr_free_inodes
        "inode-nr" => format!("{} 0\n", stats.inodes),
        // Linux /proc/sys/fs/inode-state:
        // nr_inodes nr_free_inodes preshrink nr_unused
        "inode-state" => format!("{} 0 0 0\n", stats.inodes),
        _ => return Err(VfsError::NotFound),
    })
}

impl Inode for Node {
    fn metadata(&self) -> Result<Stat, VfsError> {
        let kind = match &self.view {
            View::Root | View::Sys | View::SysFs => InodeKind::Directory,
            View::Process(w) | View::Tasks(w) | View::Fds(w, _) => {
                Self::task(w)?;
                InodeKind::Directory
            }
            View::Link(w, _) => {
                if let Some(w) = w {
                    Self::task(w)?;
                }
                InodeKind::Symlink
            }
            View::File(w, _) => {
                if let Some(w) = w {
                    Self::task(w)?;
                }
                InodeKind::Regular
            }
            View::SysFsFile(_) => InodeKind::Regular,
        };
        Ok(Stat::simple(
            self.id,
            kind,
            if kind == InodeKind::Directory {
                0o555
            } else if kind == InodeKind::Symlink {
                0o777
            } else {
                0o444
            },
            0,
        ))
    }
    fn open(&self, _: u32) -> Result<Option<Arc<dyn Inode>>, VfsError> {
        if matches!(self.view, View::File(..) | View::SysFsFile(_)) {
            Ok(Some(Snapshot::new(self.id, self.bytes()?)))
        } else {
            Ok(None)
        }
    }
    fn read_at(&self, o: u64, b: &mut [u8]) -> Result<usize, VfsError> {
        Ok(tree::read_slice(&self.bytes()?, o, b))
    }
    fn readlink(&self) -> Result<String, VfsError> {
        match &self.view {
            View::Link(None, name) => {
                let tid = sched::current().ok_or(VfsError::NotFound)?.stats().tid;
                let tgid = sched::current().ok_or(VfsError::NotFound)?.stats().tgid;
                Ok(if name == "self" {
                    format!("{}", tgid)
                } else {
                    format!("{}/task/{}", tid, tid)
                })
            }
            View::Link(Some(w), name) => {
                let task = Self::task(w)?;
                match name.as_str() {
                    "cwd" => Ok(task.fs().lock().cwd.clone()),
                    "root" => Ok(String::from("/")),
                    "exe" => {
                        let space = task.address_space().ok_or(VfsError::NotFound)?;
                        let guard = space.lock();
                        Ok(guard
                            .image
                            .as_ref()
                            .ok_or(VfsError::NotFound)?
                            .executable
                            .clone())
                    }
                    _ => {
                        let fd = name.parse::<usize>().map_err(|_| VfsError::NotFound)?;
                        Ok(task
                            .files()
                            .lock()
                            .get(fd)
                            .ok_or(VfsError::NotFound)?
                            .file
                            .abs_path
                            .clone())
                    }
                }
            }
            _ => Err(VfsError::InvalidArgument),
        }
    }
    fn lookup(&self, name: &str) -> Result<Arc<dyn Inode>, VfsError> {
        match &self.view {
            View::Root => {
                if name == "self" || name == "thread-self" {
                    return Ok(Self::make(
                        View::Link(None, String::from(name)),
                        if name == "self" { 20 } else { 21 },
                    ));
                }
                if name == "sys" {
                    return Ok(Self::make(View::Sys, 30));
                }
                #[cfg(feature = "sound")]
                if name == "asound" {
                    return Ok(crate::sound::procfs::root());
                }
                if let Some(index) = SYSTEM.iter().position(|n| *n == name) {
                    return Ok(Self::make(
                        View::File(None, String::from(name)),
                        100 + index as u64,
                    ));
                }
                let pid = name.parse::<u64>().map_err(|_| VfsError::NotFound)?;
                let task = sched::find_tgid(pid)
                    .or_else(|| sched::find_task(pid))
                    .ok_or(VfsError::NotFound)?;
                Ok(Self::make(
                    View::Process(task.downgrade()),
                    0x10000 + pid * 64,
                ))
            }
            View::Sys => {
                if name == "fs" {
                    Ok(Self::make(View::SysFs, 31))
                } else {
                    Err(VfsError::NotFound)
                }
            }
            View::SysFs => {
                let index = SYS_FS
                    .iter()
                    .position(|entry| *entry == name)
                    .ok_or(VfsError::NotFound)?;
                Ok(Self::make(
                    View::SysFsFile(String::from(name)),
                    300 + index as u64,
                ))
            }
            View::Process(w) => {
                Self::task(w)?;
                let index = PROCESS
                    .iter()
                    .position(|n| *n == name)
                    .ok_or(VfsError::NotFound)?;
                let view = match name {
                    "cwd" | "exe" | "root" => View::Link(Some(w.clone()), String::from(name)),
                    "fd" => View::Fds(w.clone(), false),
                    "fdinfo" => View::Fds(w.clone(), true),
                    "task" => View::Tasks(w.clone()),
                    _ => View::File(Some(w.clone()), String::from(name)),
                };
                Ok(Self::make(view, 0x10000 + w.tid() * 64 + index as u64 + 1))
            }
            View::Tasks(w) => {
                let task = Self::task(w)?;
                let stats = task.stats();
                let target_tid = name.parse::<u64>().map_err(|_| VfsError::NotFound)?;
                if target_tid == w.tid() || target_tid == stats.tgid || target_tid == stats.ltid {
                    return Ok(Self::make(View::Process(w.clone()), 0x10000 + w.tid() * 64));
                }
                if let Some(thread) = sched::tasks_in_group(stats.tgid)
                    .into_iter()
                    .find(|t| t.stats().ltid == target_tid || t.stats().tid == target_tid)
                {
                    return Ok(Self::make(
                        View::Process(thread.downgrade()),
                        0x10000 + thread.stats().tid * 64,
                    ));
                }
                Err(VfsError::NotFound)
            }
            View::Fds(w, info) => {
                let task = Self::task(w)?;
                let fd = name.parse::<usize>().map_err(|_| VfsError::NotFound)?;
                if task.files().lock().get(fd).is_none() {
                    return Err(VfsError::NotFound);
                }
                Ok(Self::make(
                    if *info {
                        View::File(Some(w.clone()), format!("fdinfo/{}", fd))
                    } else {
                        View::Link(Some(w.clone()), String::from(name))
                    },
                    0x1000_0000 + w.tid() * 512 + fd as u64 + if *info { 256 } else { 0 },
                ))
            }
            _ => Err(VfsError::NotADirectory),
        }
    }
    fn readdir(&self) -> Result<Vec<DirEntry>, VfsError> {
        let names: Vec<String> = match &self.view {
            View::Root => {
                let mut process_ids: Vec<u64> = sched::task_list()
                    .iter()
                    .filter(|task| task.user)
                    .map(|task| task.tgid)
                    .collect();
                process_ids.sort_unstable();
                process_ids.dedup();
                SYSTEM
                    .iter()
                    .copied()
                    .chain(["self", "thread-self", "sys"])
                    .map(String::from)
                    .chain(process_ids.into_iter().map(|pid| format!("{}", pid)))
                    .collect()
            }
            View::Sys => alloc::vec![String::from("fs")],
            View::SysFs => SYS_FS.iter().map(|name| String::from(*name)).collect(),
            View::Process(w) => {
                Self::task(w)?;
                PROCESS.iter().map(|n| String::from(*n)).collect()
            }
            View::Tasks(w) => {
                let task = Self::task(w)?;
                let stats = task.stats();
                let mut list =
                    alloc::vec![format!("{}", if stats.user { stats.ltid } else { w.tid() })];
                if stats.user {
                    for member in sched::tasks_in_group(stats.tgid) {
                        let id_str = format!("{}", member.stats().ltid);
                        if !list.contains(&id_str) {
                            list.push(id_str);
                        }
                    }
                }
                list
            }
            View::Fds(w, _) => Self::task(w)?
                .files()
                .lock()
                .snapshot()
                .iter()
                .map(|(fd, _)| format!("{}", fd))
                .collect(),
            _ => return Err(VfsError::NotADirectory),
        };
        let mut entries = alloc::vec![
            DirEntry {
                ino: self.id,
                name: String::from("."),
                kind: InodeKind::Directory
            },
            DirEntry {
                ino: 1,
                name: String::from(".."),
                kind: InodeKind::Directory
            }
        ];
        for name in names {
            if let Ok(node) = self.lookup(&name) {
                if let Ok(s) = node.metadata() {
                    entries.push(DirEntry {
                        ino: s.ino,
                        name,
                        kind: s.kind,
                    });
                }
            }
        }
        Ok(entries)
    }
}
pub struct ProcFs;
impl ProcFs {
    pub fn new() -> Self {
        Self
    }
}
impl FileSystem for ProcFs {
    fn fs_type(&self) -> &'static str {
        "proc"
    }
    fn cache_mode(&self) -> super::CacheMode {
        super::CacheMode::Revalidate
    }
    fn root_inode(&self) -> Arc<dyn Inode> {
        Node::make(View::Root, 1)
    }
    fn statfs(&self) -> Result<StatFs, VfsError> {
        Ok(StatFs {
            magic: 0x9fa0,
            block_size: 4096,
            total_blocks: 0,
            free_blocks: 0,
            total_files: sched::task_list().len() as u64,
            free_files: 0,
            max_name_len: 255,
        })
    }
}
