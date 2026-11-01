/*
 *
 *       src/ipc/netlink.rs
 *       AF_NETLINK NETLINK_KOBJECT_UEVENT sockets for device hotplug events
 *
 *       2026/10/30 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Only the uevent protocol exists. The kernel multicasts to group 1, exactly
//! like Linux; userspace device managers may rebroadcast to other groups, which
//! is how libudev-zero expects to receive events (group 4 by default).
//!
//! Each datagram is `action@devpath\0KEY=VALUE\0...`.

use crate::{
    sync::SpinLock,
    vfs::{self, Inode, InodeKind, Stat, VfsError},
};
use alloc::{
    collections::VecDeque,
    string::String,
    sync::{Arc, Weak},
    vec::Vec,
};

pub const AF_NETLINK: u64 = 16;
pub const NETLINK_KOBJECT_UEVENT: u64 = 15;
const QUEUE_LIMIT: usize = 256;
const SOL_SOCKET: u64 = 1;
const SO_RCVBUF: u64 = 8;
const SO_SNDBUF: u64 = 7;
const SO_PASSCRED: u64 = 16;
const SO_ATTACH_FILTER: u64 = 26;
const SO_RCVBUFFORCE: u64 = 33;
const SOL_NETLINK: u64 = 270;

struct Datagram {
    sender_pid: u32,
    groups: u32,
    payload: Vec<u8>,
}

struct State {
    pid: u32,
    groups: u32,
    queue: VecDeque<Datagram>,
    overflow: bool,
    nonblock: bool,
}

pub struct NetlinkSocket {
    state: SpinLock<State>,
    ino: u64,
}

static SOCKETS: SpinLock<Vec<Weak<NetlinkSocket>>> = SpinLock::new(Vec::new());

impl NetlinkSocket {
    pub fn create(nonblock: bool) -> Arc<Self> {
        let socket = Arc::new(Self {
            state: SpinLock::new(State {
                pid: 0,
                groups: 0,
                queue: VecDeque::new(),
                overflow: false,
                nonblock,
            }),
            ino: vfs::tree::ino(),
        });
        let mut sockets = SOCKETS.lock();
        sockets.retain(|weak| weak.strong_count() != 0);
        sockets.push(Arc::downgrade(&socket));
        socket
    }

    pub fn bind(&self, address: &[u8]) -> Result<(), VfsError> {
        if address.len() < 12 || u16::from_le_bytes([address[0], address[1]]) as u64 != AF_NETLINK {
            return Err(VfsError::InvalidArgument);
        }
        let pid = u32::from_le_bytes(address[4..8].try_into().unwrap());
        let groups = u32::from_le_bytes(address[8..12].try_into().unwrap());
        let mut state = self.state.lock();
        state.pid = if pid != 0 {
            pid
        } else {
            crate::sched::current().map_or(0, |task| task.stats().tgid as u32)
        };
        state.groups = groups;
        Ok(())
    }

    pub fn address(&self) -> [u8; 12] {
        let state = self.state.lock();
        encode_address(state.pid, state.groups)
    }

    /// Userspace multicast, used by device managers to rebroadcast events.
    pub fn send(&self, destination: &[u8], payload: &[u8]) -> Result<usize, VfsError> {
        if destination.len() < 12 {
            return Err(VfsError::DestinationRequired);
        }
        let groups = u32::from_le_bytes(destination[8..12].try_into().unwrap());
        if groups == 0 {
            return Err(VfsError::ConnectionRefused);
        }
        let creds = crate::sched::current().map(|task| task.credentials());
        if creds.is_some_and(|creds| creds.euid != 0) {
            return Err(VfsError::NotPermitted);
        }
        let sender = self.state.lock().pid;
        deliver(sender, groups, payload);
        Ok(payload.len())
    }

    /// Returns `(sender pid, groups, payload)`; the payload is truncated by the caller.
    pub fn receive(&self, dontwait: bool) -> Result<(u32, u32, Vec<u8>), VfsError> {
        loop {
            {
                let mut state = self.state.lock();
                if state.overflow {
                    state.overflow = false;
                    return Err(VfsError::NoBuffers);
                }
                if let Some(datagram) = state.queue.pop_front() {
                    return Ok((datagram.sender_pid, datagram.groups, datagram.payload));
                }
                if dontwait || state.nonblock {
                    return Err(VfsError::WouldBlock);
                }
            }
            if crate::signal::pending_for_current() {
                return Err(VfsError::Interrupted);
            }
            crate::sched::sleep_micros(1000);
        }
    }
}

fn encode_address(pid: u32, groups: u32) -> [u8; 12] {
    let mut raw = [0u8; 12];
    raw[0..2].copy_from_slice(&(AF_NETLINK as u16).to_le_bytes());
    raw[4..8].copy_from_slice(&pid.to_le_bytes());
    raw[8..12].copy_from_slice(&groups.to_le_bytes());
    raw
}

pub fn encoded_source(pid: u32, groups: u32) -> [u8; 12] {
    encode_address(pid, groups)
}

fn deliver(sender: u32, groups: u32, payload: &[u8]) {
    let sockets: Vec<Arc<NetlinkSocket>> =
        SOCKETS.lock().iter().filter_map(Weak::upgrade).collect();
    for socket in sockets {
        let mut state = socket.state.lock();
        if state.groups & groups == 0 {
            continue;
        }
        if state.queue.len() >= QUEUE_LIMIT {
            state.overflow = true;
            continue;
        }
        state.queue.push_back(Datagram {
            sender_pid: sender,
            groups,
            payload: payload.to_vec(),
        });
        drop(state);
        crate::ipc::wake_pollers();
    }
}

/// Kernel uevent multicast to group 1.
pub fn broadcast_uevent(action: &str, devpath: &str, properties: &[(String, String)]) {
    let mut payload = Vec::with_capacity(128);
    payload.extend_from_slice(action.as_bytes());
    payload.push(b'@');
    payload.extend_from_slice(devpath.as_bytes());
    payload.push(0);
    let mut push = |key: &str, value: &str| {
        payload.extend_from_slice(key.as_bytes());
        payload.push(b'=');
        payload.extend_from_slice(value.as_bytes());
        payload.push(0);
    };
    push("ACTION", action);
    push("DEVPATH", devpath);
    for (key, value) in properties {
        push(key, value);
    }
    deliver(0, 1, &payload);
}

impl Inode for NetlinkSocket {
    fn metadata(&self) -> Result<Stat, VfsError> {
        Ok(Stat::simple(self.ino, InodeKind::Socket, 0o777, 0))
    }

    fn poll_sources(&self, _sources: &mut crate::ipc::pollwait::PollSources) {
        // Delivery announces itself through wake_pollers.
    }

    fn poll(&self) -> u32 {
        let state = self.state.lock();
        // POLLOUT is always possible: multicast never blocks.
        4 | if state.queue.is_empty() && !state.overflow {
            0
        } else {
            1
        }
    }

    fn set_nonblock(&self, nonblock: bool) -> Result<(), VfsError> {
        self.state.lock().nonblock = nonblock;
        Ok(())
    }
}

pub fn instance(fd: u64) -> Option<Arc<NetlinkSocket>> {
    if fd > i32::MAX as u64 {
        return None;
    }
    let inode = vfs::current_fd_table()
        .lock()
        .get(fd as usize)?
        .file
        .inode
        .clone();
    let any: Arc<dyn core::any::Any + Send + Sync> = inode;
    any.downcast::<NetlinkSocket>().ok()
}

/// Options that real device managers set; none changes this implementation.
pub fn setsockopt(level: u64, name: u64) -> Result<(), VfsError> {
    match (level, name) {
        (SOL_SOCKET, SO_RCVBUF | SO_SNDBUF | SO_PASSCRED | SO_RCVBUFFORCE | SO_ATTACH_FILTER)
        | (SOL_NETLINK, _) => Ok(()),
        _ => Err(VfsError::NoProtocolOption),
    }
}
