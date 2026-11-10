/*
 *       src/device.rs
 *       Device identities, registry views and removal-safe node sessions
 *       2026/10/24 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 */
//! Registry lock -> tree locks. No device callbacks while registry lock is held.
use crate::{
    sync::SpinLock,
    user::UserAddressSpace,
    vfs::{
        Inode, InodeKind, Stat, VfsError,
        tree::{Attribute, Directory, Symlink},
    },
};
use alloc::{
    collections::{BTreeMap, VecDeque},
    format,
    string::String,
    sync::Arc,
    vec::Vec,
};
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
static MANAGER: SpinLock<Option<Manager>> = SpinLock::new(None);
static INITIALIZED: AtomicBool = AtomicBool::new(false);
static NEXT: AtomicU64 = AtomicU64::new(1);
struct Manager {
    dev: Arc<Directory>,
    sys: Arc<Directory>,
    devices: Arc<Directory>,
    classes: BTreeMap<String, Arc<Directory>>,
    records: Vec<Arc<Device>>,
    events: VecDeque<Event>,
    sequence: u64,
}
#[derive(Clone)]
pub struct Event {
    pub sequence: u64,
    pub action: &'static str,
    pub path: String,
}
pub struct Device {
    pub id: u64,
    pub name: String,
    pub subsystem: String,
    pub path: String,
    pub node: Option<String>,
    pub rdev: u64,
    pub active: AtomicBool,
    pub directory: Arc<Directory>,
    backend: Option<Arc<dyn Inode>>,
    metadata: SpinLock<Stat>,
}
struct DeviceNode {
    device: Arc<Device>,
    session: Arc<dyn Inode>,
    metadata: Arc<SpinLock<Stat>>,
}
impl DeviceNode {
    fn check(&self) -> Result<(), VfsError> {
        if self.device.active.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err(VfsError::NoDevice)
        }
    }
}
impl Inode for DeviceNode {
    fn metadata(&self) -> Result<Stat, VfsError> {
        Ok(*self.metadata.lock())
    }
    fn open(&self, flags: u32) -> Result<Option<Arc<dyn Inode>>, VfsError> {
        self.check()?;
        let session = self.session.open(flags)?.unwrap_or(self.session.clone());
        Ok(Some(Arc::new(Self {
            device: self.device.clone(),
            session,
            metadata: self.metadata.clone(),
        })))
    }
    fn read_at(&self, o: u64, b: &mut [u8]) -> Result<usize, VfsError> {
        self.check()?;
        self.session.read_at(o, b)
    }
    fn write_at(&self, o: u64, b: &[u8]) -> Result<usize, VfsError> {
        self.check()?;
        self.session.write_at(o, b)
    }
    fn ioctl(&self, c: u64, a: u64) -> Result<u64, VfsError> {
        self.check()?;
        self.session.ioctl(c, a)
    }
    fn poll_sources(&self, sources: &mut crate::ipc::pollwait::PollSources) {
        self.session.poll_sources(sources);
    }
    fn poll(&self) -> u32 {
        if self.check().is_err() {
            8 | 16
        } else {
            self.session.poll()
        }
    }
    fn mmap(
        &self,
        s: &mut UserAddressSpace,
        a: u64,
        l: u64,
        o: u64,
        p: u32,
    ) -> Result<(), VfsError> {
        self.check()?;
        self.session.mmap(s, a, l, o, p)
    }
    fn sync(&self) -> Result<(), VfsError> {
        self.check()?;
        self.session.sync()
    }
    fn seek(&self, offset: i64, whence: u32) -> Result<u64, VfsError> {
        self.check()?;
        self.session.seek(offset, whence)
    }
    fn set_nonblock(&self, nonblock: bool) -> Result<(), VfsError> {
        self.session.set_nonblock(nonblock)
    }
    #[cfg(feature = "sound")]
    fn sound_substream(&self) -> Option<alloc::sync::Arc<crate::sound::pcm::Substream>> {
        self.session.sound_substream()
    }
    fn console_vt(&self) -> Option<usize> {
        self.session.console_vt()
    }
    fn chmod(&self, mode: u32) -> Result<(), VfsError> {
        self.metadata.lock().mode = mode & 0o7777;
        Ok(())
    }
    fn chown(&self, uid: u32, gid: u32) -> Result<(), VfsError> {
        let mut m = self.metadata.lock();
        m.uid = uid;
        m.gid = gid;
        Ok(())
    }
}
fn init_empty() {
    let mut manager = MANAGER.lock();
    if manager.is_some() {
        return;
    }
    let dev = Directory::new(0o755, true);
    let sys = Directory::new(0o555, false);
    let devices = Directory::new(0o555, false);
    sys.insert("devices", devices.clone()).unwrap();
    for name in ["class", "bus", "dev", "kernel", "fs"] {
        sys.insert(name, Directory::new(0o555, false)).unwrap();
    }
    sys.lookup("kernel")
        .unwrap()
        .insert_node(
            "uevent_seqnum",
            Attribute::new(0o444, || {
                Ok(format!(
                    "{}\n",
                    MANAGER.lock().as_ref().map(|m| m.sequence).unwrap_or(0)
                )
                .into_bytes())
            }),
        )
        .unwrap();
    sys.lookup("kernel")
        .unwrap()
        .insert_node(
            "uevents",
            Attribute::new(0o444, || {
                let mut out = String::new();
                if let Some(m) = MANAGER.lock().as_ref() {
                    for ev in &m.events {
                        out.push_str(&format!(
                            "SEQNUM={}\nACTION={}\nDEVPATH={}\n\n",
                            ev.sequence, ev.action, ev.path
                        ));
                    }
                }
                Ok(out.into_bytes())
            }),
        )
        .unwrap();
    *manager = Some(Manager {
        dev,
        sys,
        devices,
        classes: BTreeMap::new(),
        records: Vec::new(),
        events: VecDeque::new(),
        sequence: 0,
    });
}
pub fn dev_root() -> Arc<Directory> {
    init_empty();
    MANAGER.lock().as_ref().unwrap().dev.clone()
}
pub fn sys_root() -> Arc<Directory> {
    init_empty();
    MANAGER.lock().as_ref().unwrap().sys.clone()
}
pub fn records() -> Vec<Arc<Device>> {
    init_empty();
    MANAGER.lock().as_ref().unwrap().records.clone()
}
fn descend(root: Arc<dyn Inode>, parts: &[&str]) -> Result<Arc<dyn Inode>, VfsError> {
    let mut node = root;
    for part in parts {
        node = node.lookup(part)?;
    }
    Ok(node)
}
struct ActiveAttr {
    device: Arc<Device>,
    inner: Arc<dyn Inode>,
}
impl Inode for ActiveAttr {
    fn metadata(&self) -> Result<Stat, VfsError> {
        if !self.device.active.load(Ordering::Acquire) {
            return Err(VfsError::NoDevice);
        }
        self.inner.metadata()
    }
    fn open(&self, flags: u32) -> Result<Option<Arc<dyn Inode>>, VfsError> {
        if !self.device.active.load(Ordering::Acquire) {
            return Err(VfsError::NoDevice);
        }
        self.inner.open(flags)
    }
    fn read_at(&self, o: u64, b: &mut [u8]) -> Result<usize, VfsError> {
        if !self.device.active.load(Ordering::Acquire) {
            return Err(VfsError::NoDevice);
        }
        self.inner.read_at(o, b)
    }
    fn write_at(&self, o: u64, b: &[u8]) -> Result<usize, VfsError> {
        if !self.device.active.load(Ordering::Acquire) {
            return Err(VfsError::NoDevice);
        }
        self.inner.write_at(o, b)
    }
    fn readlink(&self) -> Result<String, VfsError> {
        if !self.device.active.load(Ordering::Acquire) {
            return Err(VfsError::NoDevice);
        }
        self.inner.readlink()
    }
    fn truncate(&self, size: u64) -> Result<(), VfsError> {
        if !self.device.active.load(Ordering::Acquire) {
            return Err(VfsError::NoDevice);
        }
        self.inner.truncate(size)
    }
}

pub fn register(
    name: &str,
    subsystem: &str,
    node: Option<&str>,
    backend: Option<Arc<dyn Inode>>,
    attributes: Vec<(&str, Arc<dyn Inode>)>,
) -> Result<Arc<Device>, VfsError> {
    register_inner(name, subsystem, node, backend, attributes, None, Vec::new())
}

/// Like `register`, with extra `KEY=VALUE` properties for `uevent` and netlink.
pub fn register_with_uevent(
    name: &str,
    subsystem: &str,
    node: Option<&str>,
    backend: Option<Arc<dyn Inode>>,
    attributes: Vec<(&str, Arc<dyn Inode>)>,
    properties: Vec<(String, String)>,
) -> Result<Arc<Device>, VfsError> {
    register_inner(name, subsystem, node, backend, attributes, None, properties)
}

/// Register a device nested below `parent` in `/sys/devices`, as udev expects
/// for class devices such as `input0/event0`.
pub fn register_child(
    parent: &Arc<Device>,
    name: &str,
    subsystem: &str,
    node: Option<&str>,
    backend: Option<Arc<dyn Inode>>,
    attributes: Vec<(&str, Arc<dyn Inode>)>,
) -> Result<Arc<Device>, VfsError> {
    register_inner(
        name,
        subsystem,
        node,
        backend,
        attributes,
        Some(parent),
        Vec::new(),
    )
}

fn register_inner(
    name: &str,
    subsystem: &str,
    node: Option<&str>,
    backend: Option<Arc<dyn Inode>>,
    attributes: Vec<(&str, Arc<dyn Inode>)>,
    parent_device: Option<&Arc<Device>>,
    properties: Vec<(String, String)>,
) -> Result<Arc<Device>, VfsError> {
    init_empty();
    if name.is_empty() || name.contains('/') || name == "." || name == ".." {
        return Err(VfsError::InvalidArgument);
    }
    if node.is_some() && backend.is_none() {
        return Err(VfsError::NoDevice);
    }
    let directory = Directory::new(0o555, false);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let path = match parent_device {
        Some(parent) => format!("{}/{}", parent.path, name),
        None => format!("/devices/{}", name),
    };
    // Relative route from this device's sysfs directory back to /sys.
    let to_sys = "../".repeat(path.split('/').filter(|part| !part.is_empty()).count());
    let mut meta = if let Some(b) = &backend {
        b.metadata()?
    } else {
        Stat::simple(crate::vfs::tree::ino(), InodeKind::CharDevice, 0o600, 0)
    };
    meta.ino = crate::vfs::tree::ino();
    let device = Arc::new(Device {
        id,
        name: String::from(name),
        subsystem: String::from(subsystem),
        path: path.clone(),
        node: node.map(String::from),
        rdev: meta.rdev,
        active: AtomicBool::new(false),
        directory: directory.clone(),
        backend: backend.clone(),
        metadata: SpinLock::new(meta),
    });
    for (key, value) in attributes {
        let wrapped: Arc<dyn Inode> = Arc::new(ActiveAttr {
            device: device.clone(),
            inner: value,
        });
        directory.insert(key, wrapped)?;
    }
    let extra: String = properties
        .iter()
        .map(|(key, value)| format!("{}={}\n", key, value))
        .collect();
    let major = crate::vfs::abi::major(meta.rdev);
    let minor = crate::vfs::abi::minor(meta.rdev);
    let uevent_text = format!(
        "DEVPATH={}\nSUBSYSTEM={}\n{}{}",
        path,
        subsystem,
        node.map(|n| format!("DEVNAME={}\nMAJOR={}\nMINOR={}\n", n, major, minor))
            .unwrap_or_default(),
        extra
    );
    let uevent_attr = Attribute::writable(
        0o644,
        move || Ok(uevent_text.as_bytes().to_vec()),
        move |buf| {
            let s = core::str::from_utf8(buf)
                .map_err(|_| VfsError::InvalidArgument)?
                .trim();
            let action: &'static str = match s {
                "add" => "add",
                "remove" => "remove",
                "change" => "change",
                _ => return Err(VfsError::InvalidArgument),
            };
            trigger_event(id, action)
        },
    );
    directory.insert(
        "uevent",
        Arc::new(ActiveAttr {
            device: device.clone(),
            inner: uevent_attr,
        }),
    )?;
    if node.is_some() {
        directory.insert(
            "dev",
            Arc::new(ActiveAttr {
                device: device.clone(),
                inner: Attribute::text(format!("{}:{}\n", major, minor)),
            }),
        )?;
    }
    let mut manager = MANAGER.lock();
    let m = manager.as_mut().unwrap();
    if m.records.iter().any(|d| d.name == name) {
        return Err(VfsError::AlreadyExists);
    }
    let mut created_dev_dirs: Vec<(Arc<dyn Inode>, String)> = Vec::new();
    let node_parent = if let Some(node) = node {
        let parts: Vec<&str> = node.split('/').collect();
        if parts
            .iter()
            .any(|p| p.is_empty() || *p == "." || *p == "..")
        {
            return Err(VfsError::InvalidArgument);
        }
        let mut parent: Arc<dyn Inode> = m.dev.clone();
        for part in &parts[..parts.len() - 1] {
            parent = match parent.lookup(part) {
                Ok(n) => n,
                Err(VfsError::NotFound) => {
                    let dir = parent.mkdir(part, 0o755)?;
                    created_dev_dirs.push((parent.clone(), String::from(*part)));
                    dir
                }
                Err(e) => {
                    for (p, d) in created_dev_dirs.into_iter().rev() {
                        let _ = p.rmdir(&d);
                    }
                    return Err(e);
                }
            };
        }
        if parent.lookup(parts[parts.len() - 1]).is_ok() {
            for (p, d) in created_dev_dirs.into_iter().rev() {
                let _ = p.rmdir(&d);
            }
            return Err(VfsError::AlreadyExists);
        }
        Some((parent, String::from(parts[parts.len() - 1])))
    } else {
        None
    };
    if !m.classes.contains_key(subsystem) {
        let class = Directory::new(0o555, false);
        let parent = m.sys.lookup("class")?;
        parent.insert_node(subsystem, class.clone())?;
        m.classes.insert(String::from(subsystem), class);
    }
    directory.insert(
        "subsystem",
        Symlink::new(&format!(
            "{}{}/{}",
            to_sys,
            if subsystem == "pci" { "bus" } else { "class" },
            subsystem
        )),
    )?;
    if let Some(parent) = parent_device {
        parent.directory.insert(name, directory.clone())?;
        directory.insert("device", Symlink::new(".."))?;
    } else {
        m.devices.insert(name, directory.clone())?;
    }
    let class = m.classes.get(subsystem).unwrap().clone();
    class.insert(name, Symlink::new(&format!("../..{}", path)))?;
    if let Some((parent, leaf)) = node_parent {
        let wrapper: Arc<dyn Inode> = Arc::new(DeviceNode {
            device: device.clone(),
            session: backend.ok_or(VfsError::NoDevice)?,
            metadata: Arc::new(SpinLock::new(meta)),
        });
        if let Err(e) = parent.insert_node(&leaf, wrapper) {
            let _ = m.devices.remove(name);
            let _ = class.remove(name);
            for (p, d) in created_dev_dirs.into_iter().rev() {
                let _ = p.rmdir(&d);
            }
            return Err(e);
        }
    }
    if node.is_some() {
        let root = m.sys.lookup("dev")?;
        let kind = if meta.kind == InodeKind::BlockDevice {
            "block"
        } else {
            "char"
        };
        let parent = match root.lookup(kind) {
            Ok(d) => d,
            Err(_) => {
                let d = Directory::new(0o555, false);
                root.insert_node(kind, d.clone())?;
                d
            }
        };
        let key = format!("{}:{}", major, minor);
        if parent.lookup(&key).is_err() {
            parent.insert_node(&key, Symlink::new(&format!("../..{}", path)))?;
        }
    }
    if subsystem == "pci" {
        let busroot = m.sys.lookup("bus")?;
        let bus = match busroot.lookup("pci") {
            Ok(d) => d,
            Err(_) => {
                let d = Directory::new(0o555, false);
                busroot.insert_node("pci", d.clone())?;
                d
            }
        };
        let devices = match bus.lookup("devices") {
            Ok(d) => d,
            Err(_) => {
                let d = Directory::new(0o555, false);
                bus.insert_node("devices", d.clone())?;
                d
            }
        };
        devices.insert_node(name, Symlink::new(&format!("../../../devices/{}", name)))?;
    }
    if subsystem == "drm" {
        if name == "card0" || name == "renderD128" {
            if let Some(pci_dev) = m.records.iter().find(|d| d.subsystem == "pci") {
                let _ = directory.insert("device", Symlink::new(&format!("../{}", pci_dev.name)));
            }
        } else if name.starts_with("card0-") {
            let _ = directory.insert("device", Symlink::new("../card0"));
        }
    }
    device.active.store(true, Ordering::Release);
    m.records.push(device.clone());
    m.sequence += 1;
    let sequence = m.sequence;
    if m.events.len() == 256 {
        m.events.pop_front();
    }
    m.events.push_back(Event {
        sequence,
        action: "add",
        path: path.clone(),
    });
    drop(manager);
    let mut uevent: Vec<(String, String)> = alloc::vec![
        (String::from("SUBSYSTEM"), String::from(subsystem)),
        (String::from("SEQNUM"), format!("{}", sequence)),
    ];
    if let Some(node) = node {
        uevent.push((String::from("DEVNAME"), String::from(node)));
        uevent.push((String::from("MAJOR"), format!("{}", major)));
        uevent.push((String::from("MINOR"), format!("{}", minor)));
    }
    uevent.extend(properties);
    crate::ipc::netlink::broadcast_uevent("add", &path, &uevent);
    Ok(device)
}
pub fn unregister(id: u64) -> Result<(), VfsError> {
    let mut manager = MANAGER.lock();
    let m = manager.as_mut().ok_or(VfsError::NoDevice)?;
    let index = m
        .records
        .iter()
        .position(|d| d.id == id)
        .ok_or(VfsError::NotFound)?;
    let device = m.records.remove(index);
    device.active.store(false, Ordering::Release);
    let _ = m.devices.remove(&device.name);
    if device.node.is_some()
        && !m
            .records
            .iter()
            .any(|d| d.rdev == device.rdev && d.node.is_some())
    {
        let kind = if device.metadata.lock().kind == InodeKind::BlockDevice {
            "block"
        } else {
            "char"
        };
        if let Ok(parent) = descend(m.sys.clone(), &["dev", kind]) {
            let _ = parent.remove_node(&format!("{}:{}", device.rdev >> 8, device.rdev & 255));
        }
    }
    if device.subsystem == "pci" {
        if let Ok(parent) = descend(m.sys.clone(), &["bus", "pci", "devices"]) {
            let _ = parent.remove_node(&device.name);
        }
    }
    if let Some(class) = m.classes.get(&device.subsystem) {
        let _ = class.remove(&device.name);
    }
    if let Some(path) = &device.node {
        let parts: Vec<&str> = path.split('/').collect();
        if let Ok(parent) = descend(m.dev.clone(), &parts[..parts.len() - 1]) {
            let leaf = parts[parts.len() - 1];
            if parent
                .lookup(leaf)
                .and_then(|n| n.metadata())
                .is_ok_and(|s| s.ino == device.metadata.lock().ino)
            {
                let _ = parent.unlink(leaf);
            }
        }
    }
    m.sequence += 1;
    let sequence = m.sequence;
    if m.events.len() == 256 {
        m.events.pop_front();
    }
    m.events.push_back(Event {
        sequence,
        action: "remove",
        path: device.path.clone(),
    });
    let backend = device.backend.clone();
    drop(manager);
    if let Some(backend) = backend {
        backend.deactivate()?;
    }
    Ok(())
}
pub fn events_after(sequence: u64) -> Vec<Event> {
    MANAGER
        .lock()
        .as_ref()
        .map(|m| {
            m.events
                .iter()
                .filter(|e| e.sequence > sequence)
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}
pub fn init() {
    init_empty();
    if INITIALIZED.swap(true, Ordering::AcqRel) {
        return;
    }
    use crate::vfs::FileSystem;
    let legacy = crate::vfs::devfs::DevFs::new().root_inode();
    for entry in legacy.readdir().unwrap_or_default() {
        if entry.name == "." || entry.name == ".." {
            continue;
        }
        if let Ok(backend) = legacy.lookup(&entry.name) {
            let subsystem = if entry.kind == InodeKind::BlockDevice {
                "block"
            } else if matches!(
                entry.name.as_str(),
                "null" | "zero" | "full" | "random" | "urandom" | "kmsg"
            ) {
                "mem"
            } else if entry.name == "rtc0" {
                "misc"
            } else {
                "tty"
            };
            let mut attrs: Vec<(&str, Arc<dyn Inode>)> = Vec::new();
            if let Some((id, b)) = crate::block::find_device(&entry.name) {
                attrs.push((
                    "size",
                    Attribute::text(format!("{}\n", b.info().capacity_bytes() / 512)),
                ));
                attrs.push((
                    "ro",
                    Attribute::text(format!("{}\n", u8::from(b.info().read_only))),
                ));
                attrs.push((
                    "stat",
                    Attribute::new(0o444, move || {
                        let s = crate::block::accounting::stats(id);
                        Ok(format!(
                            "{} 0 {} {} {} 0 {} {} {} {} {}\n",
                            s.reads,
                            s.read_sectors,
                            s.read_us / 1000,
                            s.writes,
                            s.write_sectors,
                            s.write_us / 1000,
                            s.inflight,
                            s.busy_us / 1000,
                            s.weighted_us / 1000
                        )
                        .into_bytes())
                    }),
                ));
                if let Some(parent_disk) = entry
                    .name
                    .strip_suffix("p1")
                    .or_else(|| entry.name.strip_suffix('1').filter(|p| !p.ends_with('m')))
                {
                    attrs.push(("partition", Attribute::text("1\n")));
                    attrs.push(("device", Symlink::new(&format!("../{}", parent_disk))));
                }
            }
            let _ = register(
                &entry.name,
                subsystem,
                Some(&entry.name),
                Some(backend),
                attrs,
            );
        }
    }
    for pci in crate::pci::devices() {
        let name = format!("{}", pci.address);
        let driver =
            if pci.vendor_id == 0x1af4 && (pci.device_id == 0x1050 || pci.device_id == 0x1040) {
                Some("virtio-pci")
            } else if pci.class == 0x01 && pci.subclass == 0x06 {
                Some("ahci")
            } else if pci.class == 0x01 && pci.subclass == 0x08 {
                Some("nvme")
            } else if pci.class == 0x01 && pci.subclass == 0x01 {
                Some("ata_piix")
            } else {
                None
            };
        let address = pci.address;
        let config_attr = Attribute::new(0o444, move || {
            let config = crate::pci::config_for(address).map_err(|_| VfsError::IoError)?;
            let mut bytes = alloc::vec![0u8; 256];
            for (offset, byte) in bytes.iter_mut().enumerate() {
                *byte = config.read_u8(offset as u16).unwrap_or(0xFF);
            }
            Ok(bytes)
        });
        let mut resource_text = String::new();
        for slot in 0..6 {
            use crate::pci::Bar;
            // `start end flags`, as Linux prints; 0x200 is IORESOURCE_MEM.
            match pci.bars.get(slot).copied().flatten() {
                Some(Bar::Memory32 { base, .. }) => resource_text.push_str(&format!(
                    "0x{:016x} 0x{:016x} 0x{:016x}\n",
                    base,
                    u64::from(base) + 0xFFF,
                    0x40200u64
                )),
                Some(Bar::Memory64 { base, .. }) => resource_text.push_str(&format!(
                    "0x{:016x} 0x{:016x} 0x{:016x}\n",
                    base,
                    base + 0xFFF,
                    0x140204u64
                )),
                _ => resource_text
                    .push_str("0x0000000000000000 0x0000000000000000 0x0000000000000000\n"),
            }
        }
        let is_display = pci.class == 0x03;
        let mut attrs: Vec<(&str, Arc<dyn Inode>)> = alloc::vec![
            ("config", config_attr),
            ("resource", Attribute::text(resource_text)),
            ("irq", Attribute::text("0\n")),
            (
                "subsystem_vendor",
                Attribute::text(format!("0x{:04x}\n", pci.vendor_id))
            ),
            (
                "subsystem_device",
                Attribute::text(format!("0x{:04x}\n", pci.device_id))
            ),
            (
                "boot_vga",
                Attribute::text(if is_display { "1\n" } else { "0\n" })
            ),
            (
                "vendor",
                Attribute::text(format!("0x{:04x}\n", pci.vendor_id))
            ),
            (
                "device",
                Attribute::text(format!("0x{:04x}\n", pci.device_id))
            ),
            (
                "class",
                Attribute::text(format!(
                    "0x{:02x}{:02x}{:02x}\n",
                    pci.class, pci.subclass, pci.programming_interface
                ))
            ),
            (
                "revision",
                Attribute::text(format!("0x{:02x}\n", pci.revision))
            )
        ];
        if let Some(drv) = driver {
            attrs.push((
                "driver",
                Symlink::new(&format!("../../bus/pci/drivers/{}", drv)),
            ));
        }
        if register(&name, "pci", None, None, attrs).is_ok()
            && let Some(drv) = driver
        {
            let sys = sys_root();
            if let Ok(bus) = descend(sys, &["bus", "pci"]) {
                let drivers = match bus.lookup("drivers") {
                    Ok(d) => d,
                    Err(_) => {
                        let d = Directory::new(0o555, false);
                        let _ = bus.insert_node("drivers", d.clone());
                        d
                    }
                };
                let drv_dir = match drivers.lookup(drv) {
                    Ok(d) => d,
                    Err(_) => {
                        let d = Directory::new(0o555, false);
                        let _ = drivers.insert_node(drv, d.clone());
                        d
                    }
                };
                let _ = drv_dir.insert_node(
                    &name,
                    Symlink::new(&format!("../../../../devices/{}", name)),
                );
            }
        }
    }
}

pub fn alias(rdev: u64, kind: InodeKind, mode: u32) -> Result<Arc<dyn Inode>, VfsError> {
    let device = records()
        .into_iter()
        .find(|d| {
            d.rdev == rdev
                && d.node.is_some()
                && d.metadata.lock().kind == kind
                && d.active.load(Ordering::Acquire)
        })
        .ok_or(VfsError::NoDevice)?;
    let backend = device.backend.clone().ok_or(VfsError::NoDevice)?;
    let mut stat = backend.metadata()?;
    stat.ino = crate::vfs::tree::ino();
    stat.mode = mode & 0o7777;
    Ok(Arc::new(DeviceNode {
        device,
        session: backend,
        metadata: Arc::new(SpinLock::new(stat)),
    }))
}

pub fn trigger_event(id: u64, action: &'static str) -> Result<(), VfsError> {
    let mut manager = MANAGER.lock();
    let m = manager.as_mut().ok_or(VfsError::NoDevice)?;
    let path = m
        .records
        .iter()
        .find(|d| d.id == id && d.active.load(Ordering::Acquire))
        .ok_or(VfsError::NotFound)?
        .path
        .clone();
    m.sequence += 1;
    let sequence = m.sequence;
    if m.events.len() == 256 {
        m.events.pop_front();
    }
    m.events.push_back(Event {
        sequence,
        action,
        path,
    });
    Ok(())
}

pub fn change(id: u64) -> Result<(), VfsError> {
    trigger_event(id, "change")
}
