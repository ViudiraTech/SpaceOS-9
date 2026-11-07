/*
 *
 *       src/net/module_api.rs
 *       Network C ABI for loadable modules: netdev, NAPI and skb
 *
 *       2026/11/6 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! The adapter wraps a module's callbacks in [`NetDeviceOps`] and gives the
//! module an opaque handle. Handles are allocation-stable `Arc` pointers; using
//! a registry means a stale or forged pointer is rejected instead of
//! dereferenced.

use super::{
    dev::{self, IFF_PROMISC, NetDevice, NetDeviceConfig, NetDeviceOps, TxResult},
    ether,
    skb::SkBuff,
};
use crate::{sync::SpinLock, vfs::VfsError};
use alloc::{
    boxed::Box,
    string::{String, ToString},
    sync::{Arc, Weak},
    vec::Vec,
};
use core::{
    ffi::{c_char, c_int, c_void},
    ptr, slice,
};

#[repr(C)]
pub struct CNetOps {
    pub open: Option<extern "C" fn(*mut c_void) -> c_int>,
    pub stop: Option<extern "C" fn(*mut c_void) -> c_int>,
    pub start_xmit: Option<extern "C" fn(*mut c_void, *mut SkBuff) -> c_int>,
    pub set_rx_mode: Option<extern "C" fn(*mut c_void)>,
    pub set_mac_address: Option<extern "C" fn(*mut c_void, *const u8) -> c_int>,
    pub change_mtu: Option<extern "C" fn(*mut c_void, u32) -> c_int>,
    pub tx_timeout: Option<extern "C" fn(*mut c_void)>,
}

impl Copy for CNetOps {}
impl Clone for CNetOps {
    fn clone(&self) -> Self {
        *self
    }
}

pub struct CNetDev {
    dev: Arc<NetDevice>,
    ops: CNetOps,
    private: usize,
}

struct CAdapter {
    device: SpinLock<Weak<CNetDev>>,
}

static CDEVS: SpinLock<Vec<Weak<CNetDev>>> = SpinLock::new(Vec::new());

fn c_string<'a>(pointer: *const c_char) -> &'a [u8] {
    if pointer.is_null() {
        return b"(null)";
    }
    let mut length = 0usize;
    // SAFETY: module strings are NUL terminated; the scan is bounded.
    while length < 256 && unsafe { *pointer.add(length) as u8 } != 0 {
        length += 1;
    }
    // SAFETY: bounded by the validated NUL scan.
    unsafe { slice::from_raw_parts(pointer as *const u8, length) }
}

fn find(handle: *mut c_void) -> Option<Arc<CNetDev>> {
    if handle.is_null() {
        return None;
    }
    let devices = CDEVS.lock();
    devices
        .iter()
        .find(|weak| weak.as_ptr() as *mut c_void == handle)
        .and_then(Weak::upgrade)
}

impl CAdapter {
    fn get(&self) -> Result<Arc<CNetDev>, VfsError> {
        self.device.lock().upgrade().ok_or(VfsError::NoDevice)
    }

    fn call(&self, callback: Option<extern "C" fn(*mut c_void) -> c_int>) -> Result<(), VfsError> {
        let Some(callback) = callback else {
            return Ok(());
        };
        let device = self.get()?;
        let pointer = Arc::as_ptr(&device) as *mut c_void;
        if callback(pointer) == 0 {
            Ok(())
        } else {
            Err(VfsError::IoError)
        }
    }
}

impl NetDeviceOps for CAdapter {
    fn open(&self, _dev: &NetDevice) -> Result<(), VfsError> {
        let device = self.get()?;
        self.call(device.ops.open)
    }

    fn stop(&self, _dev: &NetDevice) -> Result<(), VfsError> {
        let device = self.get()?;
        self.call(device.ops.stop)
    }

    fn start_xmit(&self, dev: &NetDevice, skb: SkBuff) -> TxResult {
        let Ok(device) = self.get() else {
            return TxResult::Ok;
        };
        let Some(callback) = device.ops.start_xmit else {
            return TxResult::Ok;
        };
        let length = skb.len() as u64;
        let pointer = Arc::as_ptr(&device) as *mut c_void;
        let raw = Box::into_raw(Box::new(skb));
        // SAFETY: ownership of the skb transfers to the callback; on a busy
        // return the callback must have left it untouched.
        let result = callback(pointer, raw);
        if result == 0 {
            dev.stats
                .tx_packets
                .fetch_add(1, core::sync::atomic::Ordering::Relaxed);
            dev.stats
                .tx_bytes
                .fetch_add(length, core::sync::atomic::Ordering::Relaxed);
            TxResult::Ok
        } else {
            // SAFETY: contract above.
            unsafe { TxResult::Busy(*Box::from_raw(raw)) }
        }
    }

    fn set_rx_mode(&self, _dev: &NetDevice) {
        if let Ok(device) = self.get()
            && let Some(callback) = device.ops.set_rx_mode
        {
            callback(Arc::as_ptr(&device) as *mut c_void);
        }
    }

    fn set_mac_address(&self, dev: &NetDevice, address: &[u8]) -> Result<(), VfsError> {
        let device = self.get()?;
        if let Some(callback) = device.ops.set_mac_address {
            let pointer = Arc::as_ptr(&device) as *mut c_void;
            // SAFETY: the address slice outlives the call.
            if callback(pointer, address.as_ptr()) != 0 {
                return Err(VfsError::IoError);
            }
        }
        dev.set_hw_addr(address);
        Ok(())
    }

    fn change_mtu(&self, dev: &NetDevice, mtu: u32) -> Result<(), VfsError> {
        let device = self.get()?;
        if let Some(callback) = device.ops.change_mtu {
            let pointer = Arc::as_ptr(&device) as *mut c_void;
            if callback(pointer, mtu) != 0 {
                return Err(VfsError::IoError);
            }
        }
        dev.mtu.store(mtu, core::sync::atomic::Ordering::Release);
        Ok(())
    }

    fn tx_timeout(&self, _dev: &NetDevice) {
        if let Ok(device) = self.get()
            && let Some(callback) = device.ops.tx_timeout
        {
            callback(Arc::as_ptr(&device) as *mut c_void);
        }
    }
}

/// # Safety
/// `name` must be NUL terminated; `address` must point to `address_len` bytes;
/// `ops` must remain valid until the device is freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_netdev_alloc(
    name: *const c_char,
    address: *const u8,
    address_len: u32,
    private: *mut c_void,
    ops: *const CNetOps,
) -> *mut c_void {
    if address.is_null() || ops.is_null() || address_len as usize != ether::ETH_ALEN {
        return ptr::null_mut();
    }
    let name = String::from_utf8_lossy(c_string(name)).to_string();
    if name.is_empty() {
        return ptr::null_mut();
    }
    // SAFETY: address_len was validated.
    let mut mac = [0u8; ether::ETH_ALEN];
    mac.copy_from_slice(unsafe { slice::from_raw_parts(address, ether::ETH_ALEN) });
    let adapter = Arc::new(CAdapter {
        device: SpinLock::new(Weak::new()),
    });
    let Ok(device) = NetDevice::new(NetDeviceConfig::ethernet(&name, mac), adapter.clone()) else {
        return ptr::null_mut();
    };
    // SAFETY: the descriptor is module-owned and copied.
    let ops = unsafe { ptr::read(ops) };
    let netdev = Arc::new(CNetDev {
        dev: device,
        ops,
        private: private as usize,
    });
    *adapter.device.lock() = Arc::downgrade(&netdev);
    CDEVS.lock().push(Arc::downgrade(&netdev));
    Arc::into_raw(netdev) as *mut c_void
}

/// # Safety
/// `device` must come from `spk_netdev_alloc`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_netdev_register(device: *mut c_void) -> c_int {
    let Some(netdev) = find(device) else {
        return -(crate::vfs::abi::EINVAL as c_int);
    };
    match dev::register_netdev(&netdev.dev) {
        Ok(()) => 0,
        Err(error) => error.to_errno() as c_int,
    }
}

/// # Safety
/// `device` must come from `spk_netdev_alloc`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_netdev_unregister(device: *mut c_void) {
    if let Some(netdev) = find(device) {
        let _ = dev::unregister_netdev(&netdev.dev);
    }
}

/// # Safety
/// `device` must be the exact pointer returned by `spk_netdev_alloc` and must
/// not be used afterwards.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_netdev_free(device: *mut c_void) {
    if device.is_null() {
        return;
    }
    let netdev = find(device);
    CDEVS
        .lock()
        .retain(|weak| weak.as_ptr() as *mut c_void != device);
    if let Some(netdev) = &netdev
        && netdev.dev.is_registered()
    {
        let _ = dev::unregister_netdev(&netdev.dev);
    }
    drop(netdev);
    // SAFETY: the C-owned strong reference is released exactly once.
    unsafe { drop(Arc::from_raw(device as *const CNetDev)) };
}

/// # Safety
/// `device` must be a valid handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_netdev_priv(device: *mut c_void) -> *mut c_void {
    find(device).map_or(ptr::null_mut(), |netdev| netdev.private as *mut c_void)
}

/// # Safety
/// `device` must be a valid handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_netdev_ifindex(device: *mut c_void) -> u32 {
    find(device).map_or(0, |netdev| netdev.dev.ifindex())
}

/// # Safety
/// `device` must be a valid handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_netdev_mtu(device: *mut c_void) -> u32 {
    find(device).map_or(0, |netdev| netdev.dev.mtu())
}

/// # Safety
/// `device` must be a valid handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_netdev_get_flags(device: *mut c_void) -> u32 {
    find(device).map_or(0, |netdev| netdev.dev.flags())
}

/// # Safety
/// `device` must be a valid handle and `buffer` must have `length` writable
/// bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_netdev_name(
    device: *mut c_void,
    buffer: *mut c_char,
    length: u32,
) -> c_int {
    if buffer.is_null() || length == 0 {
        return -(crate::vfs::abi::EINVAL as c_int);
    }
    let Some(netdev) = find(device) else {
        return -(crate::vfs::abi::ENODEV as c_int);
    };
    let name = netdev.dev.name();
    let bytes = name.as_bytes();
    let copy = bytes.len().min(length as usize - 1);
    // SAFETY: the destination has `length` writable bytes.
    unsafe {
        ptr::copy_nonoverlapping(bytes.as_ptr(), buffer as *mut u8, copy);
        *buffer.add(copy) = 0;
    }
    copy as c_int
}

/// # Safety
/// `buffer` must have `length` writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_netdev_get_address(
    device: *mut c_void,
    buffer: *mut u8,
    length: u32,
) -> c_int {
    let Some(netdev) = find(device) else {
        return -(crate::vfs::abi::ENODEV as c_int);
    };
    let address = netdev.dev.hw_addr();
    if buffer.is_null() || (length as usize) < address.len() {
        return -(crate::vfs::abi::EINVAL as c_int);
    }
    // SAFETY: capacity validated above.
    unsafe { ptr::copy_nonoverlapping(address.as_ptr(), buffer, address.len()) };
    address.len() as c_int
}

/// # Safety
/// `device` must be a valid handle; ownership of `skb` transfers to the stack.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_netif_rx(device: *mut c_void, skb: *mut SkBuff) {
    if skb.is_null() {
        return;
    }
    // SAFETY: ownership transferred from the module.
    let skb = unsafe { *Box::from_raw(skb) };
    match find(device) {
        Some(netdev) => dev::netif_rx(&netdev.dev, skb),
        None => drop(skb),
    }
}

/// # Safety
/// `device` must be a valid handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_netif_carrier_on(device: *mut c_void) {
    if let Some(netdev) = find(device) {
        netdev.dev.carrier_on();
    }
}

/// # Safety
/// `device` must be a valid handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_netif_carrier_off(device: *mut c_void) {
    if let Some(netdev) = find(device) {
        netdev.dev.carrier_off();
    }
}

/// # Safety
/// `device` must be a valid handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_netif_stop_queue(device: *mut c_void) {
    if let Some(netdev) = find(device) {
        netdev.dev.stop_queue();
    }
}

/// # Safety
/// `device` must be a valid handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_netif_wake_queue(device: *mut c_void) {
    if let Some(netdev) = find(device) {
        netdev.dev.wake_queue();
    }
}

// ---- Socket buffers -----------------------------------------------------------

/// Allocate a buffer owned by the caller. `headroom` bytes are reserved in
/// front and `capacity` bytes are available for payload.
#[unsafe(no_mangle)]
pub extern "C" fn spk_skb_alloc(headroom: u32, capacity: u32) -> *mut SkBuff {
    let headroom = headroom as usize;
    match SkBuff::alloc(headroom, capacity as usize) {
        Some(skb) => Box::into_raw(Box::new(skb)),
        None => ptr::null_mut(),
    }
}

/// # Safety
/// `skb` must be a live buffer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_skb_put(skb: *mut SkBuff, length: u32) -> *mut u8 {
    // SAFETY: caller contract.
    let Some(skb) = (unsafe { skb.as_mut() }) else {
        return ptr::null_mut();
    };
    skb.put(length as usize)
        .map_or(ptr::null_mut(), |slice| slice.as_mut_ptr())
}

/// # Safety
/// `skb` must be a live buffer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_skb_push(skb: *mut SkBuff, length: u32) -> *mut u8 {
    // SAFETY: caller contract.
    let Some(skb) = (unsafe { skb.as_mut() }) else {
        return ptr::null_mut();
    };
    skb.push(length as usize)
        .map_or(ptr::null_mut(), |slice| slice.as_mut_ptr())
}

/// # Safety
/// `skb` must be a live buffer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_skb_pull(skb: *mut SkBuff, length: u32) -> *mut u8 {
    // SAFETY: caller contract.
    let Some(skb) = (unsafe { skb.as_mut() }) else {
        return ptr::null_mut();
    };
    skb.pull(length as usize)
        .map_or(ptr::null_mut(), |slice| slice.as_ptr() as *mut u8)
}

/// # Safety
/// `skb` must be a live buffer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_skb_reserve(skb: *mut SkBuff, length: u32) {
    if let Some(skb) = unsafe { skb.as_mut() } {
        let _ = skb.reserve(length as usize);
    }
}

/// # Safety
/// `skb` must be a live buffer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_skb_trim(skb: *mut SkBuff, length: u32) {
    if let Some(skb) = unsafe { skb.as_mut() } {
        skb.trim(length as usize);
    }
}

/// # Safety
/// `skb` must be a live buffer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_skb_len(skb: *const SkBuff) -> u32 {
    unsafe { skb.as_ref() }.map_or(0, |skb| skb.len() as u32)
}

/// # Safety
/// `skb` must be a live buffer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_skb_data(skb: *const SkBuff) -> *mut u8 {
    // SAFETY: caller contract.
    let Some(skb) = (unsafe { skb.as_ref() }) else {
        return ptr::null_mut();
    };
    skb.data().as_ptr() as *mut u8
}

/// # Safety
/// `skb` must be a live buffer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_skb_set_protocol(skb: *mut SkBuff, protocol: u16) {
    if let Some(skb) = unsafe { skb.as_mut() } {
        skb.protocol = protocol;
    }
}

/// # Safety
/// `skb` must come from `spk_skb_alloc` and not be used afterwards.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_skb_free(skb: *mut SkBuff) {
    if !skb.is_null() {
        // SAFETY: ownership returns to the kernel.
        unsafe { drop(Box::from_raw(skb)) };
    }
}

/// Strip and classify the Ethernet header (`eth_type_trans`). Returns the
/// ethertype in host byte order and leaves `skb` positioned at the network
/// header, ready for `spk_netif_rx`.
///
/// # Safety
/// `skb` must be a live buffer containing an Ethernet frame.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_eth_type_trans(skb: *mut SkBuff, device: *mut c_void) -> u16 {
    let Some(skb) = (unsafe { skb.as_mut() }) else {
        return 0;
    };
    let Some(netdev) = find(device) else {
        return 0;
    };
    let address = netdev.dev.hw_addr();
    let promiscuous = netdev.dev.flags() & IFF_PROMISC != 0;
    if !ether::type_trans(skb, &address, promiscuous) {
        return 0;
    }
    skb.protocol
}

// ---- NAPI ---------------------------------------------------------------------

type CPoll = extern "C" fn(*mut c_void, c_int) -> c_int;

pub struct CNapi {
    poll: CPoll,
    weight: usize,
    napi: SpinLock<Option<Arc<dev::Napi>>>,
}

static CNAPIS: SpinLock<Vec<Arc<CNapi>>> = SpinLock::new(Vec::new());

fn find_napi(handle: *mut c_void) -> Option<Arc<CNapi>> {
    if handle.is_null() {
        return None;
    }
    CNAPIS
        .lock()
        .iter()
        .find(|napi| Arc::as_ptr(napi) as *mut c_void == handle)
        .cloned()
}

/// # Safety
/// `device` must be a valid handle; `poll` must remain valid until deletion.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_napi_add(
    device: *mut c_void,
    poll: CPoll,
    weight: c_int,
) -> *mut c_void {
    let Some(netdev) = find(device) else {
        return ptr::null_mut();
    };
    let weight = weight.max(1) as usize;
    let cnapi = Arc::new(CNapi {
        poll,
        weight,
        napi: SpinLock::new(None),
    });
    let weak = Arc::downgrade(&cnapi);
    let closure = move |_napi: &dev::Napi, budget: usize| -> usize {
        let Some(cnapi) = weak.upgrade() else {
            return budget;
        };
        let pointer = Weak::as_ptr(&weak) as *mut c_void;
        let work = (cnapi.poll)(pointer, budget.min(c_int::MAX as usize) as c_int);
        work.max(0) as usize
    };
    let napi = netdev.dev.napi_add(weight, closure);
    *cnapi.napi.lock() = Some(napi);
    let handle = Arc::as_ptr(&cnapi) as *mut c_void;
    CNAPIS.lock().push(cnapi);
    handle
}

/// # Safety
/// `device` and `napi` must be valid handles.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_napi_del(device: *mut c_void, napi: *mut c_void) {
    let Some(netdev) = find(device) else {
        return;
    };
    let Some(cnapi) = find_napi(napi) else {
        return;
    };
    if let Some(napi) = cnapi.napi.lock().take() {
        netdev.dev.napi_del(&napi);
    }
    CNAPIS
        .lock()
        .retain(|entry| Arc::as_ptr(entry) as *mut c_void != napi);
}

/// # Safety
/// `napi` must be a handle from `spk_napi_add`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_napi_schedule(napi: *mut c_void) {
    if let Some(cnapi) = find_napi(napi)
        && let Some(napi) = cnapi.napi.lock().as_ref()
    {
        napi.schedule();
    }
}

/// # Safety
/// `napi` must be a handle from `spk_napi_add`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_napi_complete(napi: *mut c_void) {
    if let Some(cnapi) = find_napi(napi)
        && let Some(napi) = cnapi.napi.lock().as_ref()
    {
        napi.complete();
    }
}

/// # Safety
/// `napi` must be a handle from `spk_napi_add`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_napi_enable(napi: *mut c_void) {
    if let Some(cnapi) = find_napi(napi)
        && let Some(napi) = cnapi.napi.lock().as_ref()
    {
        napi.enable();
    }
}

/// # Safety
/// `napi` must be a handle from `spk_napi_add`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn spk_napi_disable(napi: *mut c_void) {
    if let Some(cnapi) = find_napi(napi)
        && let Some(napi) = cnapi.napi.lock().as_ref()
    {
        napi.disable();
    }
}

/// Weight of a registered NAPI instance, used by tests.
pub fn napi_weight(napi: *mut c_void) -> Option<usize> {
    find_napi(napi).map(|napi| napi.weight)
}
