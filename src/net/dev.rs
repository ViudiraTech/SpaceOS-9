/*
 *
 *       src/net/dev.rs
 *       Network device model: registry, ops table, queueing and rx demux
 *
 *       2026/11/6 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Mirrors `net/core/dev.c` and `include/linux/netdevice.h`.
//!
//! Lock order: `RTNL` (sleeping) -> `REGISTRY` -> per-device spin locks. No
//! driver callback runs while `REGISTRY` is held. `start_xmit` runs under the
//! device's `xmit_lock` (a spin lock, like `HARD_TX_LOCK`), so drivers must not
//! sleep inside it. `open`/`stop`/`set_*` run under `RTNL` in task context and
//! may sleep.

use super::{
    ether,
    skb::{PacketType, SkBuff},
};
use crate::{
    device,
    sched::WaitQueue,
    sync::{Mutex, MutexGuard, SpinLock},
    vfs::{VfsError, tree::Attribute},
};
use alloc::{
    boxed::Box,
    collections::{BTreeMap, VecDeque},
    format,
    string::String,
    sync::{Arc, Weak},
    vec,
    vec::Vec,
};
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};

// ---- ARPHRD / IFF / operstate constants (uapi/linux/if_arp.h, if.h) --------

pub const ARPHRD_ETHER: u16 = 1;
pub const ARPHRD_LOOPBACK: u16 = 772;
pub const ARPHRD_NONE: u16 = 0xfffe;

pub const IFF_UP: u32 = 0x1;
pub const IFF_BROADCAST: u32 = 0x2;
pub const IFF_DEBUG: u32 = 0x4;
pub const IFF_LOOPBACK: u32 = 0x8;
pub const IFF_POINTOPOINT: u32 = 0x10;
pub const IFF_NOTRAILERS: u32 = 0x20;
pub const IFF_RUNNING: u32 = 0x40;
pub const IFF_NOARP: u32 = 0x80;
pub const IFF_PROMISC: u32 = 0x100;
pub const IFF_ALLMULTI: u32 = 0x200;
pub const IFF_MULTICAST: u32 = 0x1000;
pub const IFF_LOWER_UP: u32 = 0x10000;
pub const IFF_DORMANT: u32 = 0x20000;

/// Flags userspace may change with `SIOCSIFFLAGS` / `RTM_NEWLINK`.
const IFF_USER_MASK: u32 =
    IFF_DEBUG | IFF_NOTRAILERS | IFF_NOARP | IFF_PROMISC | IFF_ALLMULTI | IFF_MULTICAST;

pub const IF_OPER_UNKNOWN: u8 = 0;
pub const IF_OPER_NOTPRESENT: u8 = 1;
pub const IF_OPER_DOWN: u8 = 2;
pub const IF_OPER_LOWERLAYERDOWN: u8 = 3;
pub const IF_OPER_TESTING: u8 = 4;
pub const IF_OPER_DORMANT: u8 = 5;
pub const IF_OPER_UP: u8 = 6;

pub const IFNAMSIZ: usize = 16;
pub const MAX_ADDR_LEN: usize = 32;

/// `netdev_max_backlog`: per-system cap on frames waiting for the rx softirq.
const MAX_BACKLOG: usize = 1000;
/// Frames the rx thread handles before yielding (`netdev_budget`).
const RX_BUDGET: usize = 300;

// ---- Statistics -----------------------------------------------------------

/// Live counters (`struct net_device_stats`, 64-bit).
#[derive(Default)]
pub struct NetStats {
    pub rx_packets: AtomicU64,
    pub tx_packets: AtomicU64,
    pub rx_bytes: AtomicU64,
    pub tx_bytes: AtomicU64,
    pub rx_errors: AtomicU64,
    pub tx_errors: AtomicU64,
    pub rx_dropped: AtomicU64,
    pub tx_dropped: AtomicU64,
    pub multicast: AtomicU64,
    pub collisions: AtomicU64,
}

/// Point-in-time copy, the layout `rtnl_link_stats64` reports.
#[derive(Clone, Copy, Default, Debug)]
pub struct StatsSnapshot {
    pub rx_packets: u64,
    pub tx_packets: u64,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub rx_errors: u64,
    pub tx_errors: u64,
    pub rx_dropped: u64,
    pub tx_dropped: u64,
    pub multicast: u64,
    pub collisions: u64,
}

impl NetStats {
    pub fn snapshot(&self) -> StatsSnapshot {
        let load = |value: &AtomicU64| value.load(Ordering::Relaxed);
        StatsSnapshot {
            rx_packets: load(&self.rx_packets),
            tx_packets: load(&self.tx_packets),
            rx_bytes: load(&self.rx_bytes),
            tx_bytes: load(&self.tx_bytes),
            rx_errors: load(&self.rx_errors),
            tx_errors: load(&self.tx_errors),
            rx_dropped: load(&self.rx_dropped),
            tx_dropped: load(&self.tx_dropped),
            multicast: load(&self.multicast),
            collisions: load(&self.collisions),
        }
    }
}

// ---- Driver interface -----------------------------------------------------

/// `netdev_tx_t`. `Busy` hands the skb back so the queue layer can retry it
/// once the driver wakes the queue; the driver must have stopped the queue.
pub enum TxResult {
    Ok,
    Busy(SkBuff),
}

/// `struct net_device_ops`. Only `start_xmit` is mandatory.
pub trait NetDeviceOps: Send + Sync {
    /// Bring the hardware up. Task context, `RTNL` held.
    fn open(&self, _dev: &NetDevice) -> Result<(), VfsError> {
        Ok(())
    }

    /// Quiesce the hardware. After return no further `start_xmit` or NAPI poll
    /// runs for this device.
    fn stop(&self, _dev: &NetDevice) -> Result<(), VfsError> {
        Ok(())
    }

    /// Transmit one frame. Runs with the xmit lock held and IRQs disabled.
    fn start_xmit(&self, dev: &NetDevice, skb: SkBuff) -> TxResult;

    /// Reprogram the receive filter after promiscuous/all-multi/multicast
    /// list changes.
    fn set_rx_mode(&self, _dev: &NetDevice) {}

    fn set_mac_address(&self, dev: &NetDevice, address: &[u8]) -> Result<(), VfsError> {
        if dev.dev_type == ARPHRD_ETHER && !ether::is_valid(address) {
            return Err(VfsError::AddressNotAvailable);
        }
        dev.set_hw_addr(address);
        Ok(())
    }

    fn change_mtu(&self, dev: &NetDevice, mtu: u32) -> Result<(), VfsError> {
        dev.mtu.store(mtu, Ordering::Release);
        Ok(())
    }

    /// Called from the watchdog when the queue has been stopped too long.
    fn tx_timeout(&self, _dev: &NetDevice) {}

    /// Override statistics; default reads `NetDevice::stats`.
    fn get_stats(&self, _dev: &NetDevice) -> Option<StatsSnapshot> {
        None
    }
}

// ---- Device ---------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
enum RegState {
    Uninitialized = 0,
    Registered = 1,
    Unregistering = 2,
    Unregistered = 3,
}

/// Parameters for [`NetDevice::new`] (`alloc_netdev` + `setup`).
pub struct NetDeviceConfig {
    /// Name or template; `%d` is replaced by the lowest free unit number.
    pub name: String,
    pub dev_type: u16,
    pub hw_addr: Vec<u8>,
    pub broadcast: Vec<u8>,
    pub mtu: u32,
    pub min_mtu: u32,
    pub max_mtu: u32,
    pub flags: u32,
    pub tx_queue_len: u32,
}

impl NetDeviceConfig {
    /// `ether_setup` defaults for a NIC.
    pub fn ethernet(name: &str, mac: [u8; ether::ETH_ALEN]) -> Self {
        Self {
            name: String::from(name),
            dev_type: ARPHRD_ETHER,
            hw_addr: mac.to_vec(),
            broadcast: ether::BROADCAST.to_vec(),
            mtu: ether::ETH_DATA_LEN,
            min_mtu: 68,
            max_mtu: 1500,
            flags: IFF_BROADCAST | IFF_MULTICAST,
            tx_queue_len: 1000,
        }
    }
}

pub struct NetDevice {
    ifindex: AtomicU32,
    name: SpinLock<String>,
    pub dev_type: u16,
    pub addr_len: u8,
    hw_addr: SpinLock<Vec<u8>>,
    perm_addr: Vec<u8>,
    pub broadcast: Vec<u8>,
    pub mtu: AtomicU32,
    pub min_mtu: u32,
    pub max_mtu: AtomicU32,
    /// Persistent `IFF_*` bits (everything except UP/RUNNING/LOWER_UP/DORMANT).
    gflags: AtomicU32,
    up: AtomicBool,
    carrier: AtomicBool,
    dormant: AtomicBool,
    pub tx_queue_len: AtomicU32,
    pub stats: NetStats,
    ops: Arc<dyn NetDeviceOps>,
    txq: SpinLock<VecDeque<SkBuff>>,
    xmit_lock: SpinLock<()>,
    queue_stopped: AtomicBool,
    reg_state: AtomicU8,
    sys: SpinLock<Option<Arc<device::Device>>>,
    multicast: SpinLock<Vec<Vec<u8>>>,
    napis: SpinLock<Vec<Arc<Napi>>>,
    me: Weak<NetDevice>,
}

impl NetDevice {
    pub fn new(config: NetDeviceConfig, ops: Arc<dyn NetDeviceOps>) -> Result<Arc<Self>, VfsError> {
        if config.hw_addr.len() > MAX_ADDR_LEN
            || config.name.is_empty()
            || config.name.len() >= IFNAMSIZ
            || config.min_mtu > config.max_mtu
            || config.mtu < config.min_mtu
            || config.mtu > config.max_mtu
        {
            return Err(VfsError::InvalidArgument);
        }
        Ok(Arc::new_cyclic(|me| Self {
            ifindex: AtomicU32::new(0),
            name: SpinLock::new(config.name),
            dev_type: config.dev_type,
            addr_len: config.hw_addr.len() as u8,
            perm_addr: config.hw_addr.clone(),
            hw_addr: SpinLock::new(config.hw_addr),
            broadcast: config.broadcast,
            mtu: AtomicU32::new(config.mtu),
            min_mtu: config.min_mtu,
            max_mtu: AtomicU32::new(config.max_mtu),
            gflags: AtomicU32::new(config.flags),
            up: AtomicBool::new(false),
            carrier: AtomicBool::new(true),
            dormant: AtomicBool::new(false),
            tx_queue_len: AtomicU32::new(config.tx_queue_len),
            stats: NetStats::default(),
            ops,
            txq: SpinLock::new(VecDeque::new()),
            xmit_lock: SpinLock::new(()),
            queue_stopped: AtomicBool::new(false),
            reg_state: AtomicU8::new(RegState::Uninitialized as u8),
            sys: SpinLock::new(None),
            multicast: SpinLock::new(Vec::new()),
            napis: SpinLock::new(Vec::new()),
            me: me.clone(),
        }))
    }

    pub fn ifindex(&self) -> u32 {
        self.ifindex.load(Ordering::Acquire)
    }

    pub fn name(&self) -> String {
        self.name.lock().clone()
    }

    pub fn hw_addr(&self) -> Vec<u8> {
        self.hw_addr.lock().clone()
    }

    pub fn perm_addr(&self) -> &[u8] {
        &self.perm_addr
    }

    pub fn set_hw_addr(&self, address: &[u8]) {
        if address.len() == self.addr_len as usize {
            *self.hw_addr.lock() = address.to_vec();
        }
    }

    pub fn mtu(&self) -> u32 {
        self.mtu.load(Ordering::Acquire)
    }

    pub fn is_up(&self) -> bool {
        self.up.load(Ordering::Acquire)
    }

    pub fn carrier_ok(&self) -> bool {
        self.carrier.load(Ordering::Acquire)
    }

    pub fn queue_stopped(&self) -> bool {
        self.queue_stopped.load(Ordering::Acquire)
    }

    pub fn arc(&self) -> Arc<NetDevice> {
        self.me
            .upgrade()
            .expect("net device used after its last reference")
    }

    /// `dev_get_flags`: stored bits plus the computed UP/RUNNING/LOWER_UP.
    pub fn flags(&self) -> u32 {
        let mut flags = self.gflags.load(Ordering::Acquire);
        if self.is_up() {
            flags |= IFF_UP;
            if self.carrier_ok() && !self.dormant.load(Ordering::Acquire) {
                flags |= IFF_RUNNING;
            }
        }
        if self.carrier_ok() {
            flags |= IFF_LOWER_UP;
        }
        if self.dormant.load(Ordering::Acquire) {
            flags |= IFF_DORMANT;
        }
        flags
    }

    /// RFC 2863 operational state (`default_operstate`).
    pub fn operstate(&self) -> u8 {
        if self.reg_state() != RegState::Registered {
            return IF_OPER_NOTPRESENT;
        }
        if !self.carrier_ok() {
            return if self.is_up() {
                IF_OPER_LOWERLAYERDOWN
            } else {
                IF_OPER_DOWN
            };
        }
        if self.dormant.load(Ordering::Acquire) {
            return IF_OPER_DORMANT;
        }
        if self.is_up() {
            IF_OPER_UP
        } else {
            IF_OPER_DOWN
        }
    }

    pub fn stats_snapshot(&self) -> StatsSnapshot {
        self.ops
            .get_stats(self)
            .unwrap_or_else(|| self.stats.snapshot())
    }

    pub fn multicast_list(&self) -> Vec<Vec<u8>> {
        self.multicast.lock().clone()
    }

    fn reg_state(&self) -> RegState {
        match self.reg_state.load(Ordering::Acquire) {
            1 => RegState::Registered,
            2 => RegState::Unregistering,
            3 => RegState::Unregistered,
            _ => RegState::Uninitialized,
        }
    }

    pub fn is_registered(&self) -> bool {
        self.reg_state() == RegState::Registered
    }

    // ---- carrier / queue state, callable from IRQ context --------------

    /// `netif_carrier_on`. Safe from interrupt context; the netlink
    /// notification is deferred to the net thread.
    pub fn carrier_on(&self) {
        if !self.carrier.swap(true, Ordering::AcqRel) {
            defer(Deferred::Carrier(self.arc()));
        }
    }

    pub fn carrier_off(&self) {
        if self.carrier.swap(false, Ordering::AcqRel) {
            defer(Deferred::Carrier(self.arc()));
        }
    }

    pub fn set_dormant(&self, dormant: bool) {
        if self.dormant.swap(dormant, Ordering::AcqRel) != dormant {
            defer(Deferred::Carrier(self.arc()));
        }
    }

    pub fn stop_queue(&self) {
        self.queue_stopped.store(true, Ordering::Release);
    }

    /// `netif_wake_queue`: restart transmission and run the queue.
    pub fn wake_queue(&self) {
        if self.queue_stopped.swap(false, Ordering::AcqRel) {
            defer(Deferred::Xmit(self.arc()));
        }
    }

    /// `netif_start_queue`: allow transmission without kicking the queue.
    pub fn start_queue(&self) {
        self.queue_stopped.store(false, Ordering::Release);
    }

    // ---- NAPI ------------------------------------------------------------

    /// `netif_napi_add`. `poll(budget)` returns the number of frames handled;
    /// returning `budget` means more work remains, returning less requires the
    /// poll function to have called [`Napi::complete`].
    pub fn napi_add(
        &self,
        weight: usize,
        poll: impl Fn(&Napi, usize) -> usize + Send + Sync + 'static,
    ) -> Arc<Napi> {
        let napi = Arc::new(Napi {
            state: AtomicU32::new(NAPI_DISABLED),
            weight,
            poll: Box::new(poll),
            dev: self.me.clone(),
        });
        self.napis.lock().push(napi.clone());
        napi
    }

    pub fn napi_del(&self, napi: &Arc<Napi>) {
        napi.disable();
        self.napis.lock().retain(|other| !Arc::ptr_eq(other, napi));
    }

    fn napis(&self) -> Vec<Arc<Napi>> {
        self.napis.lock().clone()
    }
}

// ---- NAPI -----------------------------------------------------------------

const NAPI_SCHED: u32 = 1;
const NAPI_DISABLED: u32 = 2;

pub struct Napi {
    state: AtomicU32,
    pub weight: usize,
    poll: Box<dyn Fn(&Napi, usize) -> usize + Send + Sync>,
    dev: Weak<NetDevice>,
}

impl Napi {
    pub fn device(&self) -> Option<Arc<NetDevice>> {
        self.dev.upgrade()
    }

    /// `napi_enable`: allow scheduling.
    pub fn enable(&self) {
        self.state.fetch_and(!NAPI_DISABLED, Ordering::AcqRel);
    }

    /// `napi_disable`: forbid scheduling and wait for a running poll to end.
    /// Task context only.
    pub fn disable(&self) {
        self.state.fetch_or(NAPI_DISABLED, Ordering::AcqRel);
        while self.state.load(Ordering::Acquire) & NAPI_SCHED != 0 {
            crate::sched::sleep_micros(1000);
        }
    }

    /// `napi_schedule`: queue a poll. Safe from interrupt context. Returns
    /// whether this call scheduled it.
    pub fn schedule(self: &Arc<Self>) -> bool {
        let mut state = self.state.load(Ordering::Acquire);
        loop {
            if state & (NAPI_SCHED | NAPI_DISABLED) != 0 {
                return false;
            }
            match self.state.compare_exchange_weak(
                state,
                state | NAPI_SCHED,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(current) => state = current,
            }
        }
        NAPI_LIST.lock().push_back(self.clone());
        kick();
        true
    }

    /// `napi_complete_done`: the poll function is finished; the driver may now
    /// re-enable its interrupt.
    pub fn complete(&self) {
        self.state.fetch_and(!NAPI_SCHED, Ordering::AcqRel);
    }
}

// ---- Registry and notifiers -----------------------------------------------

struct Registry {
    by_index: BTreeMap<u32, Arc<NetDevice>>,
    next_index: u32,
}

static REGISTRY: SpinLock<Registry> = SpinLock::new(Registry {
    by_index: BTreeMap::new(),
    next_index: 1,
});

static RTNL: Mutex<()> = Mutex::new(());

/// Global configuration lock (`rtnl_lock`). Netlink configuration handlers and
/// every state-changing device operation hold it.
pub fn rtnl_lock() -> MutexGuard<'static, ()> {
    RTNL.lock()
}

/// Events delivered to [`register_notifier`] callbacks (`NETDEV_*`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetdevEvent {
    Register,
    Unregister,
    Up,
    Down,
    ChangeAddr,
    ChangeMtu,
    ChangeName,
    ChangeFlags,
    /// Carrier or dormant state changed (`NETDEV_CHANGE`).
    Change,
}

pub type Notifier = fn(NetdevEvent, &Arc<NetDevice>);

static NOTIFIERS: SpinLock<Vec<Notifier>> = SpinLock::new(Vec::new());

/// `register_netdevice_notifier`. Callbacks run in task context; most hold
/// `RTNL` (config path), carrier changes do not.
pub fn register_notifier(notifier: Notifier) {
    NOTIFIERS.lock().push(notifier);
}

fn notify(event: NetdevEvent, dev: &Arc<NetDevice>) {
    let list = NOTIFIERS.lock().clone();
    for notifier in list {
        notifier(event, dev);
    }
}

pub fn by_index(ifindex: u32) -> Option<Arc<NetDevice>> {
    REGISTRY.lock().by_index.get(&ifindex).cloned()
}

pub fn by_name(name: &str) -> Option<Arc<NetDevice>> {
    REGISTRY
        .lock()
        .by_index
        .values()
        .find(|dev| *dev.name.lock() == name)
        .cloned()
}

/// Snapshot of all registered devices ordered by ifindex.
pub fn devices() -> Vec<Arc<NetDevice>> {
    REGISTRY.lock().by_index.values().cloned().collect()
}

fn resolve_name(registry: &Registry, template: &str) -> Result<String, VfsError> {
    let taken = |candidate: &str| {
        registry
            .by_index
            .values()
            .any(|dev| *dev.name.lock() == candidate)
    };
    if let Some(position) = template.find("%d") {
        for unit in 0..4096u32 {
            let candidate = format!(
                "{}{}{}",
                &template[..position],
                unit,
                &template[position + 2..]
            );
            if candidate.len() < IFNAMSIZ && !taken(&candidate) {
                return Ok(candidate);
            }
        }
        return Err(VfsError::NoSpace);
    }
    if taken(template) {
        return Err(VfsError::AlreadyExists);
    }
    Ok(String::from(template))
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() < IFNAMSIZ
        && name != "."
        && name != ".."
        && !name
            .bytes()
            .any(|b| b == b'/' || b == 0 || b.is_ascii_whitespace())
}

/// `register_netdev`: assign ifindex and name, publish in `/sys/class/net`,
/// emit the uevent and notify subscribers.
pub fn register_netdev(dev: &Arc<NetDevice>) -> Result<(), VfsError> {
    let _rtnl = rtnl_lock();
    if dev.reg_state() != RegState::Uninitialized {
        return Err(VfsError::InvalidArgument);
    }
    let template = dev.name();
    if !template.contains("%d") && !valid_name(&template) {
        return Err(VfsError::InvalidArgument);
    }
    {
        let mut registry = REGISTRY.lock();
        let name = resolve_name(&registry, &template)?;
        let mut index = registry.next_index;
        while registry.by_index.contains_key(&index) || index == 0 {
            index = index.wrapping_add(1);
        }
        registry.next_index = index.wrapping_add(1);
        *dev.name.lock() = name;
        dev.ifindex.store(index, Ordering::Release);
        registry.by_index.insert(index, dev.clone());
    }
    let name = dev.name();
    let attributes = sysfs_attributes(dev);
    let properties = vec![
        (String::from("INTERFACE"), name.clone()),
        (String::from("IFINDEX"), format!("{}", dev.ifindex())),
    ];
    match device::register_with_uevent(&name, "net", None, None, attributes, properties) {
        Ok(sys) => *dev.sys.lock() = Some(sys),
        Err(error) => {
            REGISTRY.lock().by_index.remove(&dev.ifindex());
            return Err(error);
        }
    }
    dev.reg_state
        .store(RegState::Registered as u8, Ordering::Release);
    notify(NetdevEvent::Register, dev);
    Ok(())
}

/// `unregister_netdev`: close the device, remove it everywhere and wait until
/// no transmit or poll can still reference it.
pub fn unregister_netdev(dev: &Arc<NetDevice>) -> Result<(), VfsError> {
    let _rtnl = rtnl_lock();
    if dev.reg_state() != RegState::Registered {
        return Err(VfsError::InvalidArgument);
    }
    dev.reg_state
        .store(RegState::Unregistering as u8, Ordering::Release);
    if dev.is_up() {
        close_locked(dev);
    }
    for napi in dev.napis() {
        napi.disable();
    }
    REGISTRY.lock().by_index.remove(&dev.ifindex());
    notify(NetdevEvent::Unregister, dev);
    if let Some(sys) = dev.sys.lock().take() {
        let _ = device::unregister(sys.id);
    }
    // A concurrent xmit holds the lock; once we take it none can start.
    drop(dev.xmit_lock.lock());
    dev.txq.lock().clear();
    dev.reg_state
        .store(RegState::Unregistered as u8, Ordering::Release);
    Ok(())
}

fn sysfs_attributes(dev: &Arc<NetDevice>) -> Vec<(&'static str, Arc<dyn crate::vfs::Inode>)> {
    fn text(
        dev: &Arc<NetDevice>,
        render: impl Fn(&NetDevice) -> String + Send + Sync + 'static,
    ) -> Arc<dyn crate::vfs::Inode> {
        let weak = Arc::downgrade(dev);
        Attribute::new(0o444, move || {
            let dev = weak.upgrade().ok_or(VfsError::NoDevice)?;
            Ok(render(&dev).into_bytes())
        })
    }
    vec![
        ("address", text(dev, |d| format_mac(&d.hw_addr()) + "\n")),
        ("addr_len", text(dev, |d| format!("{}\n", d.addr_len))),
        ("broadcast", text(dev, |d| format_mac(&d.broadcast) + "\n")),
        (
            "carrier",
            text(dev, |d| format!("{}\n", d.carrier_ok() as u8)),
        ),
        ("flags", text(dev, |d| format!("{:#x}\n", d.flags()))),
        ("ifindex", text(dev, |d| format!("{}\n", d.ifindex()))),
        ("mtu", text(dev, |d| format!("{}\n", d.mtu()))),
        (
            "operstate",
            text(dev, |d| format!("{}\n", operstate_name(d.operstate()))),
        ),
        (
            "tx_queue_len",
            text(dev, |d| {
                format!("{}\n", d.tx_queue_len.load(Ordering::Relaxed))
            }),
        ),
        ("type", text(dev, |d| format!("{}\n", d.dev_type))),
    ]
}

pub fn format_mac(address: &[u8]) -> String {
    let mut out = String::new();
    for (index, byte) in address.iter().enumerate() {
        if index != 0 {
            out.push(':');
        }
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

pub fn operstate_name(state: u8) -> &'static str {
    match state {
        IF_OPER_NOTPRESENT => "notpresent",
        IF_OPER_DOWN => "down",
        IF_OPER_LOWERLAYERDOWN => "lowerlayerdown",
        IF_OPER_TESTING => "testing",
        IF_OPER_DORMANT => "dormant",
        IF_OPER_UP => "up",
        _ => "unknown",
    }
}

// ---- Configuration (RTNL held by the caller) ---------------------------

/// `dev_open`.
pub fn open_locked(dev: &Arc<NetDevice>) -> Result<(), VfsError> {
    if !dev.is_registered() {
        return Err(VfsError::NoDevice);
    }
    if dev.is_up() {
        return Ok(());
    }
    dev.queue_stopped.store(false, Ordering::Release);
    for napi in dev.napis() {
        napi.enable();
    }
    if let Err(error) = dev.ops.open(dev) {
        for napi in dev.napis() {
            napi.disable();
        }
        return Err(error);
    }
    dev.up.store(true, Ordering::Release);
    dev.ops.set_rx_mode(dev);
    notify(NetdevEvent::Up, dev);
    Ok(())
}

/// `dev_close`.
pub fn close_locked(dev: &Arc<NetDevice>) {
    if !dev.is_up() {
        return;
    }
    dev.up.store(false, Ordering::Release);
    dev.stop_queue();
    for napi in dev.napis() {
        napi.disable();
    }
    let _ = dev.ops.stop(dev);
    dev.txq.lock().clear();
    notify(NetdevEvent::Down, dev);
}

pub fn dev_open(dev: &Arc<NetDevice>) -> Result<(), VfsError> {
    let _rtnl = rtnl_lock();
    open_locked(dev)
}

pub fn dev_close(dev: &Arc<NetDevice>) {
    let _rtnl = rtnl_lock();
    close_locked(dev);
}

/// `dev_change_flags`: apply user-settable bits and bring the device up/down.
pub fn change_flags_locked(dev: &Arc<NetDevice>, flags: u32) -> Result<(), VfsError> {
    let old = dev.gflags.load(Ordering::Acquire);
    let new = (old & !IFF_USER_MASK) | (flags & IFF_USER_MASK);
    dev.gflags.store(new, Ordering::Release);
    if (old ^ new) & (IFF_PROMISC | IFF_ALLMULTI | IFF_MULTICAST) != 0 && dev.is_up() {
        dev.ops.set_rx_mode(dev);
    }
    if (old ^ new) != 0 {
        notify(NetdevEvent::ChangeFlags, dev);
    }
    if flags & IFF_UP != 0 {
        open_locked(dev)?;
    } else {
        close_locked(dev);
    }
    Ok(())
}

pub fn change_mtu_locked(dev: &Arc<NetDevice>, mtu: u32) -> Result<(), VfsError> {
    if mtu < dev.min_mtu || mtu > dev.max_mtu.load(Ordering::Acquire) {
        return Err(VfsError::InvalidArgument);
    }
    if mtu == dev.mtu() {
        return Ok(());
    }
    dev.ops.change_mtu(dev, mtu)?;
    notify(NetdevEvent::ChangeMtu, dev);
    Ok(())
}

pub fn set_mac_locked(dev: &Arc<NetDevice>, address: &[u8]) -> Result<(), VfsError> {
    if address.len() != dev.addr_len as usize {
        return Err(VfsError::InvalidArgument);
    }
    dev.ops.set_mac_address(dev, address)?;
    notify(NetdevEvent::ChangeAddr, dev);
    Ok(())
}

pub fn rename_locked(dev: &Arc<NetDevice>, new_name: &str) -> Result<(), VfsError> {
    if dev.is_up() {
        return Err(VfsError::Busy);
    }
    if !valid_name(new_name) {
        return Err(VfsError::InvalidArgument);
    }
    {
        let registry = REGISTRY.lock();
        if registry
            .by_index
            .values()
            .any(|other| !Arc::ptr_eq(other, dev) && *other.name.lock() == new_name)
        {
            return Err(VfsError::AlreadyExists);
        }
        *dev.name.lock() = String::from(new_name);
    }
    notify(NetdevEvent::ChangeName, dev);
    Ok(())
}

/// `dev_mc_add`: add a link-layer multicast filter entry.
pub fn mc_add(dev: &Arc<NetDevice>, address: &[u8]) -> Result<(), VfsError> {
    if address.len() != dev.addr_len as usize || !ether::is_multicast(address) {
        return Err(VfsError::InvalidArgument);
    }
    {
        let mut list = dev.multicast.lock();
        if list.iter().any(|entry| entry == address) {
            return Ok(());
        }
        list.push(address.to_vec());
    }
    if dev.is_up() {
        dev.ops.set_rx_mode(dev);
    }
    Ok(())
}

pub fn mc_del(dev: &Arc<NetDevice>, address: &[u8]) {
    dev.multicast.lock().retain(|entry| entry != address);
    if dev.is_up() {
        dev.ops.set_rx_mode(dev);
    }
}

// ---- Transmit path ----------------------------------------------------------

/// `dev_queue_xmit`: enqueue on the device's FIFO qdisc and run it. Drops the
/// frame (and counts it) when the device is down or the queue is full.
pub fn queue_xmit(dev: &Arc<NetDevice>, mut skb: SkBuff) -> Result<(), VfsError> {
    if !dev.is_up() {
        dev.stats.tx_dropped.fetch_add(1, Ordering::Relaxed);
        return Err(VfsError::NoDevice);
    }
    skb.ifindex = dev.ifindex();
    skb.pkt_type = PacketType::Outgoing;
    {
        let mut queue = dev.txq.lock();
        let limit = dev.tx_queue_len.load(Ordering::Relaxed).max(1) as usize;
        if queue.len() >= limit {
            drop(queue);
            dev.stats.tx_dropped.fetch_add(1, Ordering::Relaxed);
            return Err(VfsError::NoBuffers);
        }
        queue.push_back(skb);
    }
    run_queue(dev);
    Ok(())
}

/// `qdisc_run` for a pfifo: drain while the driver accepts frames. A CPU that
/// loses the `try_lock` race relies on the holder re-checking the queue after
/// it releases the lock, so a frame pushed before the failed attempt is never
/// stranded.
fn run_queue(dev: &Arc<NetDevice>) {
    loop {
        if dev.queue_stopped() || !dev.is_up() {
            return;
        }
        let Some(guard) = dev.xmit_lock.try_lock() else {
            return;
        };
        let Some(skb) = dev.txq.lock().pop_front() else {
            drop(guard);
            if dev.txq.lock().is_empty() {
                return;
            }
            continue;
        };
        match dev.ops.start_xmit(dev, skb) {
            TxResult::Ok => {}
            TxResult::Busy(skb) => {
                if dev.queue_stopped() {
                    dev.txq.lock().push_front(skb);
                } else {
                    // The driver broke the contract (Busy without stopping the
                    // queue); dropping avoids a transmit livelock.
                    dev.stats.tx_dropped.fetch_add(1, Ordering::Relaxed);
                }
                drop(guard);
                return;
            }
        }
        drop(guard);
    }
}

// ---- Receive path and softirq thread -----------------------------------

/// Handler for one ethertype (`struct packet_type`).
pub type PacketHandler = fn(&Arc<NetDevice>, SkBuff);

static PTYPES: SpinLock<Vec<(u16, PacketHandler)>> = SpinLock::new(Vec::new());
/// Taps receive every frame regardless of protocol (`ETH_P_ALL`).
static TAPS: SpinLock<Vec<PacketHandler>> = SpinLock::new(Vec::new());

/// `dev_add_pack`.
pub fn register_packet_type(ethertype: u16, handler: PacketHandler) {
    if ethertype == ether::ETH_P_ALL {
        TAPS.lock().push(handler);
    } else {
        PTYPES.lock().push((ethertype, handler));
    }
}

struct Backlog {
    queue: VecDeque<(Arc<NetDevice>, SkBuff)>,
}

static BACKLOG: SpinLock<Backlog> = SpinLock::new(Backlog {
    queue: VecDeque::new(),
});
static NAPI_LIST: SpinLock<VecDeque<Arc<Napi>>> = SpinLock::new(VecDeque::new());

enum Deferred {
    Carrier(Arc<NetDevice>),
    Xmit(Arc<NetDevice>),
}

static DEFERRED: SpinLock<VecDeque<Deferred>> = SpinLock::new(VecDeque::new());
static PENDING: AtomicBool = AtomicBool::new(false);
static WORKER_STARTED: AtomicBool = AtomicBool::new(false);
static WORKER_WAIT: SpinLock<Option<WaitQueue>> = SpinLock::new(None);

fn defer(item: Deferred) {
    DEFERRED.lock().push_back(item);
    kick();
}

fn kick() {
    PENDING.store(true, Ordering::Release);
    if WORKER_STARTED.load(Ordering::Acquire) {
        let queue = WORKER_WAIT.lock().clone();
        if let Some(queue) = queue {
            queue.wake_all();
        }
    }
}

/// `netif_rx`: hand a received frame to the protocol layers from a context
/// that must not run protocol code (interrupt handlers, loopback xmit).
pub fn netif_rx(dev: &Arc<NetDevice>, mut skb: SkBuff) {
    if !dev.is_up() {
        dev.stats.rx_dropped.fetch_add(1, Ordering::Relaxed);
        return;
    }
    skb.ifindex = dev.ifindex();
    skb.timestamp_us = crate::time::uptime_micros();
    let accepted = {
        let mut backlog = BACKLOG.lock();
        if backlog.queue.len() >= MAX_BACKLOG {
            false
        } else {
            backlog.queue.push_back((dev.clone(), skb));
            true
        }
    };
    if accepted {
        kick();
    } else {
        dev.stats.rx_dropped.fetch_add(1, Ordering::Relaxed);
    }
}

/// `netif_receive_skb`: demultiplex by ethertype. Frames nobody wants are
/// dropped silently, as Linux does.
fn receive_skb(dev: &Arc<NetDevice>, skb: SkBuff) {
    let taps = TAPS.lock().clone();
    for tap in taps {
        if let Some(copy) = skb.duplicate() {
            tap(dev, copy);
        }
    }
    let handler = PTYPES
        .lock()
        .iter()
        .find(|(ethertype, _)| *ethertype == skb.protocol)
        .map(|(_, handler)| *handler);
    if let Some(handler) = handler {
        handler(dev, skb);
    }
}

fn run_deferred() -> bool {
    let mut did = false;
    loop {
        let Some(item) = DEFERRED.lock().pop_front() else {
            return did;
        };
        did = true;
        match item {
            Deferred::Carrier(dev) => {
                if dev.is_registered() {
                    notify(NetdevEvent::Change, &dev);
                }
            }
            Deferred::Xmit(dev) => run_queue(&dev),
        }
    }
}

/// One pass of the rx softirq (`net_rx_action`): NAPI polls first, then the
/// non-NAPI backlog, bounded by `RX_BUDGET` frames.
fn net_rx_action() -> bool {
    let mut did = run_deferred();
    let mut budget = RX_BUDGET;
    loop {
        let Some(napi) = NAPI_LIST.lock().pop_front() else {
            break;
        };
        did = true;
        let quota = napi.weight.min(budget);
        let work = (napi.poll)(&napi, quota);
        budget -= work.min(budget);
        if work >= quota && napi.state.load(Ordering::Acquire) & NAPI_SCHED != 0 {
            // Budget exhausted: poll again later, behind other devices.
            NAPI_LIST.lock().push_back(napi);
        }
        if budget == 0 {
            break;
        }
    }
    while budget > 0 {
        let Some((dev, skb)) = BACKLOG.lock().queue.pop_front() else {
            break;
        };
        did = true;
        budget -= 1;
        dev.stats.rx_packets.fetch_add(1, Ordering::Relaxed);
        dev.stats
            .rx_bytes
            .fetch_add(skb.len() as u64, Ordering::Relaxed);
        receive_skb(&dev, skb);
    }
    if budget == 0 {
        // More may remain; stay runnable.
        PENDING.store(true, Ordering::Release);
    }
    did
}

/// Spawn the rx/softirq thread. Call once after the scheduler is ready.
pub fn start_worker() {
    if WORKER_STARTED.swap(true, Ordering::AcqRel) {
        return;
    }
    let queue = WaitQueue::new();
    *WORKER_WAIT.lock() = Some(queue.clone());
    let result = crate::sched::spawn(
        move || {
            loop {
                queue.wait_until(|| PENDING.load(Ordering::Acquire));
                PENDING.store(false, Ordering::Release);
                net_rx_action();
            }
        },
        0,
    );
    if result.is_err() {
        WORKER_STARTED.store(false, Ordering::Release);
        crate::kwarn!("net: rx thread could not start");
    }
}

/// Run pending softirq work synchronously. Used by self-tests that execute
/// before the worker thread exists.
pub fn process_pending() {
    while net_rx_action() {}
}
