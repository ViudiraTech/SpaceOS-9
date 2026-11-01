/*
 *       src/ipc/mod.rs
 *       Core IPC objects: eventfd and AF_UNIX socket system calls
 *       2026/10/25 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 */

//! Core IPC objects.  This module owns the Linux syscall surface for `eventfd2`
//! and AF_UNIX sockets; the socket state machine lives in `unix`.

pub mod epoll;
pub mod futex;
pub mod inotify;
pub mod memfd;
pub mod netlink;
pub mod shm;
pub mod signalfd;
pub mod pollwait;
pub mod timerfd;

pub use pollwait::{block_briefly, poll_kick, wake_object, wake_pollers};
pub mod unix;

use crate::{
    sched,
    sync::SpinLock,
    user::uaccess,
    vfs::{
        self, FdEntry, Inode, InodeKind, OpenFile, Stat, VfsError,
        abi::{self, O_CLOEXEC, O_NONBLOCK},
    },
};
use alloc::{string::String, sync::Arc, vec::Vec};

const EFD_SEMAPHORE: u32 = 1;
const MAX_COUNTER: u64 = u64::MAX - 1;

pub struct EventFd {
    counter: SpinLock<u64>,
    semaphore: bool,
    nonblock: bool,
    ino: u64,
}

impl Inode for EventFd {
    fn metadata(&self) -> Result<Stat, VfsError> {
        Ok(Stat::simple(self.ino, InodeKind::CharDevice, 0o600, 0))
    }

    fn read_at(&self, _offset: u64, buf: &mut [u8]) -> Result<usize, VfsError> {
        if buf.len() < 8 {
            return Err(VfsError::InvalidArgument);
        }
        loop {
            let mut counter = self.counter.lock();
            if *counter != 0 {
                let value = if self.semaphore {
                    *counter -= 1;
                    1
                } else {
                    let value = *counter;
                    *counter = 0;
                    value
                };
                drop(counter);
                wake_object(self as *const Self);
                buf[..8].copy_from_slice(&value.to_le_bytes());
                return Ok(8);
            }
            drop(counter);
            if self.nonblock {
                return Err(VfsError::WouldBlock);
            }
            if crate::signal::pending_for_current() {
                return Err(VfsError::Interrupted);
            }
            sched::sleep_micros(1000);
        }
    }

    fn write_at(&self, _offset: u64, buf: &[u8]) -> Result<usize, VfsError> {
        if buf.len() < 8 {
            return Err(VfsError::InvalidArgument);
        }
        let value = u64::from_le_bytes(buf[..8].try_into().unwrap());
        if value == u64::MAX {
            return Err(VfsError::InvalidArgument);
        }
        loop {
            let mut counter = self.counter.lock();
            if counter
                .checked_add(value)
                .is_some_and(|next| next <= MAX_COUNTER)
            {
                *counter += value;
                drop(counter);
                wake_object(self as *const Self);
                return Ok(8);
            }
            drop(counter);
            if self.nonblock {
                return Err(VfsError::WouldBlock);
            }
            if crate::signal::pending_for_current() {
                return Err(VfsError::Interrupted);
            }
            sched::sleep_micros(1000);
        }
    }

    fn poll_sources(&self, sources: &mut pollwait::PollSources) {
        sources.object(self as *const Self);
    }

    fn poll(&self) -> u32 {
        let counter = *self.counter.lock();
        let mut events = 0;
        if counter > 0 {
            events |= unix::POLLIN;
        }
        if counter < MAX_COUNTER {
            events |= unix::POLLOUT;
        }
        events
    }
}

pub fn eventfd2(initval: u32, flags: u32) -> i64 {
    if flags & !(EFD_SEMAPHORE | O_CLOEXEC | O_NONBLOCK) != 0 {
        return -abi::EINVAL;
    }
    let nonblock = flags & O_NONBLOCK != 0;
    let inode = Arc::new(EventFd {
        counter: SpinLock::new(initval as u64),
        semaphore: flags & EFD_SEMAPHORE != 0,
        nonblock,
        ino: vfs::tree::ino(),
    });
    let file = Arc::new(OpenFile::new(
        inode,
        String::from("anon_inode:[eventfd]"),
        abi::O_RDWR | if nonblock { O_NONBLOCK } else { 0 },
    ));
    let fd = match vfs::current_fd_table().lock().alloc_fd(
        0,
        FdEntry {
            file,
            cloexec: flags & O_CLOEXEC != 0,
        },
    ) {
        Ok(fd) => fd,
        Err(error) => return error.to_errno(),
    };
    fd as i64
}

// ---- AF_UNIX system call surface ------------------------------------------

const MSG_PEEK: u64 = 2;
const MSG_CTRUNC: i32 = 0x20;
const MSG_TRUNC: u64 = 0x20;
const MSG_DONTWAIT: u64 = 0x40;
const MSG_EOR: u64 = 0x80;
const MSG_WAITALL: u64 = 0x100;
const MSG_NOSIGNAL: u64 = 0x4000;
const MSG_MORE: u64 = 0x8000;
const MSG_CMSG_CLOEXEC: u64 = 0x4000_0000;

const SEND_FLAGS: u64 = MSG_NOSIGNAL | MSG_DONTWAIT | MSG_EOR | MSG_MORE;
const RECV_FLAGS: u64 = MSG_PEEK | MSG_DONTWAIT | MSG_TRUNC | MSG_WAITALL | MSG_CMSG_CLOEXEC;
const MAX_SCM_FDS: usize = 253;
const CMSG_HEADER: usize = 16;
const MAX_IOV_TOTAL: usize = 256 * 1024;

fn u32_at(bytes: &[u8], offset: usize) -> Result<u32, VfsError> {
    Ok(u32::from_le_bytes(
        bytes
            .get(offset..offset + 4)
            .ok_or(VfsError::InvalidArgument)?
            .try_into()
            .unwrap(),
    ))
}

fn u64_at(bytes: &[u8], offset: usize) -> Result<u64, VfsError> {
    Ok(u64::from_le_bytes(
        bytes
            .get(offset..offset + 8)
            .ok_or(VfsError::InvalidArgument)?
            .try_into()
            .unwrap(),
    ))
}

fn socket_inode(fd: u64) -> Result<Arc<unix::UnixSocket>, VfsError> {
    if fd > i32::MAX as u64 {
        return Err(VfsError::BadFileDescriptor);
    }
    let inode = vfs::current_fd_table()
        .lock()
        .get(fd as usize)
        .ok_or(VfsError::BadFileDescriptor)?
        .file
        .inode
        .clone();
    let any: Arc<dyn core::any::Any + Send + Sync> = inode;
    any.downcast::<unix::UnixSocket>()
        .map_err(|_| VfsError::NotASocket)
}

fn current_cwd() -> String {
    vfs::current_cwd()
}

fn read_sockaddr(pointer: u64, length: u64) -> Result<unix::UnixAddress, VfsError> {
    if pointer == 0 || length == 0 {
        return Err(VfsError::InvalidArgument);
    }
    let length = usize::try_from(length).map_err(|_| VfsError::InvalidArgument)?;
    if length > 110 {
        return Err(VfsError::InvalidArgument);
    }
    let bytes = uaccess::read_vec(pointer, length, 110)?;
    let cwd = current_cwd();
    unix::decode_sockaddr(&bytes, &cwd)
}

/// `sockaddr` output follows Linux: copy at most the caller's capacity but
/// always report the full encoded length.
fn write_sockaddr(
    pointer: u64,
    length_pointer: u64,
    address: &unix::UnixAddress,
) -> Result<(), VfsError> {
    if pointer == 0 {
        return Ok(());
    }
    if length_pointer == 0 {
        return Err(VfsError::InvalidArgument);
    }
    let capacity = uaccess::read_value::<u32>(length_pointer)? as usize;
    let encoded = address.encode();
    let copy = capacity.min(encoded.len());
    uaccess::write(pointer, &encoded[..copy])?;
    uaccess::write_value(length_pointer, &(encoded.len() as u32))?;
    Ok(())
}

fn alloc_file(file: Arc<OpenFile>, cloexec: bool) -> Result<i32, VfsError> {
    vfs::current_fd_table()
        .lock()
        .alloc_fd(0, FdEntry { file, cloexec })
        .map(|fd| fd as i32)
}

fn socket_file(
    socket: Arc<unix::UnixSocket>,
    cloexec: bool,
    nonblock: bool,
) -> Result<i32, VfsError> {
    socket.set_nonblock(nonblock);
    let flags = abi::O_RDWR | if nonblock { O_NONBLOCK } else { 0 };
    alloc_file(unix::open_file(socket, flags), cloexec)
}

fn socket_from_kind(kind: u64) -> Result<(unix::SocketKind, bool, bool), i64> {
    let raw = kind as u32;
    let base = raw & !(unix::SOCK_CLOEXEC | unix::SOCK_NONBLOCK);
    let kind = unix::SocketKind::from_raw(base).map_err(|_| -abi::ESOCKTNOSUPPORT)?;
    Ok((
        kind,
        raw & unix::SOCK_CLOEXEC != 0,
        raw & unix::SOCK_NONBLOCK != 0,
    ))
}

pub fn socket(domain: u64, kind: u64, protocol: u64) -> i64 {
    if domain == netlink::AF_NETLINK {
        return netlink_socket(kind, protocol);
    }
    if domain != unix::AF_UNIX as u64 {
        return -abi::EAFNOSUPPORT;
    }
    if protocol != 0 {
        return -abi::EPROTONOSUPPORT;
    }
    let (kind, cloexec, nonblock) = match socket_from_kind(kind) {
        Ok(value) => value,
        Err(error) => return error,
    };
    let socket = unix::UnixSocket::new_socket(kind, nonblock);
    match socket_file(socket, cloexec, nonblock) {
        Ok(fd) => fd as i64,
        Err(error) => error.to_errno(),
    }
}

pub fn socketpair(domain: u64, kind: u64, protocol: u64, address: u64) -> i64 {
    if domain != unix::AF_UNIX as u64 {
        return -abi::EAFNOSUPPORT;
    }
    if protocol != 0 {
        return -abi::EPROTONOSUPPORT;
    }
    let (kind, cloexec, nonblock) = match socket_from_kind(kind) {
        Ok(value) => value,
        Err(error) => return error,
    };
    let (left, right) = match unix::UnixSocket::pair(kind, nonblock) {
        Ok(pair) => pair,
        Err(error) => return error.to_errno(),
    };
    let flags = abi::O_RDWR | if nonblock { O_NONBLOCK } else { 0 };
    let left = unix::open_file(left, flags);
    let right = unix::open_file(right, flags);
    let table_arc = vfs::current_fd_table();
    let (left_fd, right_fd) = {
        let mut table = table_arc.lock();
        let left_fd = match table.alloc_fd(
            0,
            FdEntry {
                file: left,
                cloexec,
            },
        ) {
            Ok(fd) => fd,
            Err(error) => return error.to_errno(),
        };
        let right_fd = match table.alloc_fd(
            0,
            FdEntry {
                file: right,
                cloexec,
            },
        ) {
            Ok(fd) => fd,
            Err(error) => {
                let _ = table.close(left_fd);
                return error.to_errno();
            }
        };
        (left_fd, right_fd)
    };
    let pair = [left_fd as i32, right_fd as i32];
    if let Err(error) = uaccess::write_value(address, &pair) {
        let mut table = table_arc.lock();
        let _ = table.close(left_fd);
        let _ = table.close(right_fd);
        return error.to_errno();
    }
    0
}

fn netlink_socket(kind: u64, protocol: u64) -> i64 {
    const SOCK_RAW: u32 = 3;
    const SOCK_DGRAM: u32 = 2;
    let raw = kind as u32;
    let base = raw & !(unix::SOCK_CLOEXEC | unix::SOCK_NONBLOCK);
    if base != SOCK_RAW && base != SOCK_DGRAM {
        return -abi::ESOCKTNOSUPPORT;
    }
    if protocol != netlink::NETLINK_KOBJECT_UEVENT {
        return -abi::EPROTONOSUPPORT;
    }
    let nonblock = raw & unix::SOCK_NONBLOCK != 0;
    let socket = netlink::NetlinkSocket::create(nonblock);
    let flags = abi::O_RDWR | if nonblock { O_NONBLOCK } else { 0 };
    let file = Arc::new(OpenFile::new(socket, String::from("socket:netlink"), flags));
    match alloc_file(file, raw & unix::SOCK_CLOEXEC != 0) {
        Ok(fd) => fd as i64,
        Err(error) => error.to_errno(),
    }
}

/// Deliver a queued netlink datagram into a user buffer and source address.
fn netlink_receive(
    socket: &Arc<netlink::NetlinkSocket>,
    buffer: u64,
    capacity: usize,
    flags: u64,
    source: u64,
    source_length: u64,
) -> Result<(usize, usize), VfsError> {
    let (pid, groups, payload) = socket.receive(flags & MSG_DONTWAIT != 0)?;
    let copied = capacity.min(payload.len());
    uaccess::write(buffer, &payload[..copied])?;
    if source != 0 {
        let raw = netlink::encoded_source(pid, groups);
        let room = uaccess::read_value::<u32>(source_length)? as usize;
        uaccess::write(source, &raw[..room.min(raw.len())])?;
        uaccess::write_value(source_length, &(raw.len() as u32))?;
    }
    Ok((copied, payload.len()))
}

pub fn bind(fd: u64, address: u64, length: u64) -> i64 {
    if let Some(socket) = netlink::instance(fd) {
        let result =
            uaccess::read_vec(address, length as usize, 128).and_then(|raw| socket.bind(&raw));
        return match result {
            Ok(()) => 0,
            Err(error) => error.to_errno(),
        };
    }
    let socket = match socket_inode(fd) {
        Ok(socket) => socket,
        Err(error) => return error.to_errno(),
    };
    let address = match read_sockaddr(address, length) {
        Ok(address) => address,
        Err(error) => return error.to_errno(),
    };
    match socket.bind(address) {
        Ok(()) => 0,
        Err(error) => error.to_errno(),
    }
}

pub fn listen(fd: u64, backlog: u64) -> i64 {
    let socket = match socket_inode(fd) {
        Ok(socket) => socket,
        Err(error) => return error.to_errno(),
    };
    match socket.listen(backlog as u32) {
        Ok(()) => 0,
        Err(error) => error.to_errno(),
    }
}

pub fn connect(fd: u64, address: u64, length: u64) -> i64 {
    let socket = match socket_inode(fd) {
        Ok(socket) => socket,
        Err(error) => return error.to_errno(),
    };
    let address = match read_sockaddr(address, length) {
        Ok(address) => address,
        Err(error) => return error.to_errno(),
    };
    let target = match unix::lookup(&address) {
        Ok(target) => target,
        Err(error) => return error.to_errno(),
    };
    match socket.connect(&target, address) {
        Ok(()) => 0,
        Err(error) => error.to_errno(),
    }
}

pub fn accept4(fd: u64, address: u64, length: u64, flags: u64) -> i64 {
    if flags & !(unix::SOCK_CLOEXEC as u64 | unix::SOCK_NONBLOCK as u64) != 0 {
        return -abi::EINVAL;
    }
    let socket = match socket_inode(fd) {
        Ok(socket) => socket,
        Err(error) => return error.to_errno(),
    };
    let nonblock = flags & unix::SOCK_NONBLOCK as u64 != 0;
    let cloexec = flags & unix::SOCK_CLOEXEC as u64 != 0;
    let peer = match socket.accept(nonblock) {
        Ok(peer) => peer,
        Err(error) => return error.to_errno(),
    };
    let peer_address = peer.peer_address().unwrap_or(unix::UnixAddress::Unnamed);
    let accepted = match socket_file(peer, cloexec, nonblock) {
        Ok(fd) => fd,
        Err(error) => return error.to_errno(),
    };
    if let Err(error) = write_sockaddr(address, length, &peer_address) {
        let _ = vfs::current_fd_table().lock().close(accepted as usize);
        return error.to_errno();
    }
    accepted as i64
}

pub fn getsockname(fd: u64, address: u64, length: u64) -> i64 {
    if let Some(socket) = netlink::instance(fd) {
        let raw = socket.address();
        let result = uaccess::read_value::<u32>(length).and_then(|room| {
            uaccess::write(address, &raw[..(room as usize).min(raw.len())])?;
            uaccess::write_value(length, &(raw.len() as u32))
        });
        return match result {
            Ok(()) => 0,
            Err(error) => error.to_errno(),
        };
    }
    let socket = match socket_inode(fd) {
        Ok(socket) => socket,
        Err(error) => return error.to_errno(),
    };
    match write_sockaddr(address, length, &socket.local_address()) {
        Ok(()) => 0,
        Err(error) => error.to_errno(),
    }
}

pub fn getpeername(fd: u64, address: u64, length: u64) -> i64 {
    let socket = match socket_inode(fd) {
        Ok(socket) => socket,
        Err(error) => return error.to_errno(),
    };
    let Some(peer) = socket.peer_address() else {
        return -abi::ENOTCONN;
    };
    match write_sockaddr(address, length, &peer) {
        Ok(()) => 0,
        Err(error) => error.to_errno(),
    }
}

pub fn shutdown(fd: u64, how: u32) -> i64 {
    let socket = match socket_inode(fd) {
        Ok(socket) => socket,
        Err(error) => return error.to_errno(),
    };
    match socket.shutdown(how) {
        Ok(()) => 0,
        Err(error) => error.to_errno(),
    }
}

struct SendControl {
    descriptors: Vec<Arc<OpenFile>>,
    credentials: Option<unix::Credentials>,
}

fn parse_control(control: &[u8]) -> Result<SendControl, VfsError> {
    let mut result = SendControl {
        descriptors: Vec::new(),
        credentials: None,
    };
    let mut offset = 0usize;
    let mut saw_rights = false;
    let mut saw_credentials = false;
    while control.len().saturating_sub(offset) >= CMSG_HEADER {
        let length = u64_at(control, offset)? as usize;
        let level = u32_at(control, offset + 8)? as i32;
        let kind = u32_at(control, offset + 12)? as i32;
        if length < CMSG_HEADER || length > control.len() - offset {
            return Err(VfsError::InvalidArgument);
        }
        if level == unix::SOL_SOCKET {
            match kind {
                unix::SCM_RIGHTS => {
                    if saw_rights || !(length - CMSG_HEADER).is_multiple_of(4) {
                        return Err(VfsError::InvalidArgument);
                    }
                    saw_rights = true;
                    let count = (length - CMSG_HEADER) / 4;
                    if count > MAX_SCM_FDS {
                        return Err(VfsError::InvalidArgument);
                    }
                    let table = vfs::current_fd_table();
                    let table = table.lock();
                    for index in 0..count {
                        let fd = u32_at(control, offset + CMSG_HEADER + index * 4)? as i32;
                        if fd < 0 {
                            return Err(VfsError::BadFileDescriptor);
                        }
                        let entry = table.get(fd as usize).ok_or(VfsError::BadFileDescriptor)?;
                        result.descriptors.push(entry.file);
                    }
                }
                unix::SCM_CREDENTIALS => {
                    if saw_credentials || length != CMSG_HEADER + 12 {
                        return Err(VfsError::InvalidArgument);
                    }
                    saw_credentials = true;
                    let credentials =
                        unix::Credentials::decode(&control[offset + CMSG_HEADER..offset + length])?;
                    let current = unix::current_credentials();
                    if (credentials.pid != 0 && credentials.pid != current.pid)
                        || credentials.uid != current.uid
                        || credentials.gid != current.gid
                    {
                        return Err(VfsError::PermissionDenied);
                    }
                    result.credentials = Some(current);
                }
                // Unknown SOL_SOCKET control messages are ignored, matching
                // Linux's treatment of control data it does not implement.
                _ => {}
            }
        }
        let aligned = (length + 7) & !7;
        if aligned >= control.len() - offset {
            offset = control.len();
        } else {
            offset += aligned;
        }
    }
    if result.descriptors.len() > MAX_SCM_FDS {
        return Err(VfsError::InvalidArgument);
    }
    Ok(result)
}

fn read_iovecs(address: u64, count: u64) -> Result<Vec<(u64, usize)>, VfsError> {
    if count > 1024 {
        return Err(VfsError::InvalidArgument);
    }
    let bytes = uaccess::read_vec(
        address,
        (count as usize)
            .checked_mul(16)
            .ok_or(VfsError::InvalidArgument)?,
        64 * 1024,
    )?;
    let mut result = Vec::new();
    let mut total = 0usize;
    for entry in bytes.chunks_exact(16) {
        let base = u64_at(entry, 0)?;
        let length = u64_at(entry, 8)?;
        let length = usize::try_from(length).map_err(|_| VfsError::InvalidArgument)?;
        total = total
            .checked_add(length)
            .filter(|&size| size <= MAX_IOV_TOTAL)
            .ok_or(VfsError::NoSpace)?;
        result.push((base, length));
    }
    Ok(result)
}

fn read_payload(iovecs: &[(u64, usize)]) -> Result<Vec<u8>, VfsError> {
    let total = iovecs.iter().map(|(_, length)| *length).sum::<usize>();
    let mut payload = Vec::with_capacity(total);
    for &(base, length) in iovecs {
        payload.extend_from_slice(&uaccess::read_vec(base, length, MAX_IOV_TOTAL)?);
    }
    Ok(payload)
}

fn write_payload(iovecs: &[(u64, usize)], payload: &[u8]) -> Result<(), VfsError> {
    let mut written = 0usize;
    for &(base, length) in iovecs {
        if written >= payload.len() {
            break;
        }
        let count = length.min(payload.len() - written);
        if count != 0 {
            uaccess::write(base, &payload[written..written + count])?;
            written += count;
        }
    }
    Ok(())
}

fn send_with_control(
    socket: &Arc<unix::UnixSocket>,
    destination: Option<&unix::UnixAddress>,
    payload: &[u8],
    control: &SendControl,
    flags: u64,
) -> Result<usize, VfsError> {
    let dontwait = flags & MSG_DONTWAIT != 0;
    if let Some(address) = destination {
        if socket.kind() != unix::SocketKind::Dgram {
            return Err(VfsError::AlreadyConnected);
        }
        if address.is_unnamed() {
            return Err(VfsError::DestinationRequired);
        }
        let target = unix::lookup(address)?;
        socket.send_to(
            &target,
            address.clone(),
            payload,
            &control.descriptors,
            control.credentials,
            dontwait,
        )
    } else {
        if socket.kind() == unix::SocketKind::Dgram && !socket.has_peer() {
            return Err(VfsError::DestinationRequired);
        }
        socket.send(payload, &control.descriptors, control.credentials, dontwait)
    }
}

pub fn sendmsg(fd: u64, message: u64, flags: u64) -> i64 {
    if let Some(socket) = netlink::instance(fd) {
        let result = (|| -> Result<usize, VfsError> {
            let header = read_msghdr(message)?;
            let destination = uaccess::read_vec(header.name, header.namelen as usize, 128)?;
            let mut payload = Vec::new();
            for index in 0..header.iovlen.min(1024) {
                let vector = uaccess::read_value::<abi::IoVec>(header.iov + index * 16)?;
                payload.extend_from_slice(&uaccess::read_vec(
                    vector.iov_base as u64,
                    vector.iov_len as usize,
                    65536,
                )?);
            }
            socket.send(&destination, &payload)
        })();
        return match result {
            Ok(count) => count as i64,
            Err(error) => error.to_errno(),
        };
    }
    if flags & !SEND_FLAGS != 0 {
        return -abi::EINVAL;
    }
    let header = match read_msghdr(message) {
        Ok(header) => header,
        Err(error) => return error.to_errno(),
    };
    let destination = if header.name != 0 && header.namelen != 0 {
        match read_sockaddr(header.name, header.namelen as u64) {
            Ok(address) => Some(address),
            Err(error) => return error.to_errno(),
        }
    } else {
        None
    };
    let iovecs = match read_iovecs(header.iov, header.iovlen) {
        Ok(iovecs) => iovecs,
        Err(error) => return error.to_errno(),
    };
    let payload = match read_payload(&iovecs) {
        Ok(payload) => payload,
        Err(error) => return error.to_errno(),
    };
    let control_bytes = if header.controllen == 0 {
        Vec::new()
    } else {
        match uaccess::read_vec(header.control, header.controllen as usize, MAX_IOV_TOTAL) {
            Ok(bytes) => bytes,
            Err(error) => return error.to_errno(),
        }
    };
    let control = match parse_control(&control_bytes) {
        Ok(control) => control,
        Err(error) => return error.to_errno(),
    };
    let socket = match socket_inode(fd) {
        Ok(socket) => socket,
        Err(error) => return error.to_errno(),
    };
    match send_with_control(&socket, destination.as_ref(), &payload, &control, flags) {
        Ok(count) => count as i64,
        Err(error) => error.to_errno(),
    }
}

pub fn sendto(
    fd: u64,
    buffer: u64,
    length: u64,
    flags: u64,
    address: u64,
    address_len: u64,
) -> i64 {
    if flags & !SEND_FLAGS != 0 {
        return -abi::EINVAL;
    }
    if length > MAX_IOV_TOTAL as u64 {
        return -abi::EMSGSIZE;
    }
    let payload = match uaccess::read_vec(buffer, length as usize, MAX_IOV_TOTAL) {
        Ok(payload) => payload,
        Err(error) => return error.to_errno(),
    };
    let destination = if address != 0 && address_len != 0 {
        match read_sockaddr(address, address_len) {
            Ok(address) => Some(address),
            Err(error) => return error.to_errno(),
        }
    } else {
        None
    };
    let socket = match socket_inode(fd) {
        Ok(socket) => socket,
        Err(error) => return error.to_errno(),
    };
    let control = SendControl {
        descriptors: Vec::new(),
        credentials: None,
    };
    match send_with_control(&socket, destination.as_ref(), &payload, &control, flags) {
        Ok(count) => count as i64,
        Err(error) => error.to_errno(),
    }
}

struct MsgHdr {
    name: u64,
    namelen: u32,
    iov: u64,
    iovlen: u64,
    control: u64,
    controllen: u64,
}

fn read_msghdr(address: u64) -> Result<MsgHdr, VfsError> {
    let bytes = uaccess::read_vec(address, 56, 56)?;
    Ok(MsgHdr {
        name: u64_at(&bytes, 0)?,
        namelen: u32_at(&bytes, 8)?,
        iov: u64_at(&bytes, 16)?,
        iovlen: u64_at(&bytes, 24)?,
        control: u64_at(&bytes, 32)?,
        controllen: u64_at(&bytes, 40)?,
    })
}

fn receive_into(
    socket: &Arc<unix::UnixSocket>,
    total: usize,
    flags: u64,
) -> Result<unix::Received, VfsError> {
    let peek = flags & MSG_PEEK != 0;
    let dontwait = flags & MSG_DONTWAIT != 0;
    let mut received = socket.receive(total, peek, dontwait)?;
    if flags & MSG_WAITALL != 0
        && !peek
        && socket.kind() == unix::SocketKind::Stream
        && !received.eof
        && !received.bytes.is_empty()
    {
        while received.bytes.len() < total {
            match socket.receive(total - received.bytes.len(), false, dontwait) {
                Ok(more) => {
                    if more.eof || more.bytes.is_empty() {
                        received.eof = true;
                        break;
                    }
                    received.bytes.extend_from_slice(&more.bytes);
                    if received.descriptors.is_empty() {
                        received.descriptors = more.descriptors;
                    }
                    if received.credentials.is_none() {
                        received.credentials = more.credentials;
                    }
                    received.truncated |= more.truncated;
                }
                Err(VfsError::WouldBlock) => break,
                Err(error) => return Err(error),
            }
        }
    }
    Ok(received)
}

fn pad_control(control: &mut Vec<u8>) {
    while control.len() % 8 != 0 {
        control.push(0);
    }
}

fn install_rights(
    received: &mut unix::Received,
    control_pointer: u64,
    capacity: usize,
    cloexec: bool,
) -> Result<(Vec<u8>, bool), VfsError> {
    let mut output = Vec::new();
    let mut truncated = false;
    if received.descriptors.is_empty() {
        return Ok((output, truncated));
    }
    let max_fds = if control_pointer != 0 && capacity >= CMSG_HEADER {
        (capacity - CMSG_HEADER) / 4
    } else {
        0
    };
    let take = received.descriptors.len().min(max_fds);
    let mut installed = Vec::new();
    if take != 0 {
        let table = vfs::current_fd_table();
        let mut table = table.lock();
        for descriptor in received.descriptors.drain(..take) {
            match table.alloc_fd(
                0,
                FdEntry {
                    file: descriptor,
                    cloexec,
                },
            ) {
                Ok(fd) => installed.push(fd as i32),
                // Linux scm_detach_fds installs as many descriptors as fit,
                // closes the rest and sets MSG_CTRUNC instead of failing.
                Err(_) => {
                    truncated = true;
                    break;
                }
            }
        }
        drop(table);
    }
    // Descriptors that did not fit are closed when the vector drops.
    if !received.descriptors.is_empty() {
        truncated = true;
    }
    if !installed.is_empty() {
        output.extend_from_slice(&((CMSG_HEADER + installed.len() * 4) as u64).to_le_bytes());
        output.extend_from_slice(&unix::SOL_SOCKET.to_le_bytes());
        output.extend_from_slice(&unix::SCM_RIGHTS.to_le_bytes());
        for fd in &installed {
            output.extend_from_slice(&fd.to_le_bytes());
        }
        pad_control(&mut output);
    }
    Ok((output, truncated))
}

pub fn recvmsg(fd: u64, message: u64, flags: u64) -> i64 {
    if let Some(socket) = netlink::instance(fd) {
        const MSG_TRUNC_OUT: u32 = 0x20;
        let result = (|| -> Result<usize, VfsError> {
            let header = read_msghdr(message)?;
            let vector = uaccess::read_value::<abi::IoVec>(header.iov)?;
            let (pid, groups, payload) = socket.receive(flags & MSG_DONTWAIT != 0)?;
            let copied = (vector.iov_len as usize).min(payload.len());
            uaccess::write(vector.iov_base as u64, &payload[..copied])?;
            if header.name != 0 {
                let raw = netlink::encoded_source(pid, groups);
                let room = (header.namelen as usize).min(raw.len());
                uaccess::write(header.name, &raw[..room])?;
                uaccess::write_value(message + 8, &(raw.len() as u32))?;
            }
            uaccess::write_value(message + 40, &0u64)?;
            let out = if copied < payload.len() {
                MSG_TRUNC_OUT
            } else {
                0
            };
            uaccess::write_value(message + 48, &out)?;
            Ok(if flags & MSG_TRUNC != 0 {
                payload.len()
            } else {
                copied
            })
        })();
        return match result {
            Ok(count) => count as i64,
            Err(error) => error.to_errno(),
        };
    }
    if flags & !RECV_FLAGS != 0 {
        return -abi::EINVAL;
    }
    let header = match read_msghdr(message) {
        Ok(header) => header,
        Err(error) => return error.to_errno(),
    };
    let iovecs = match read_iovecs(header.iov, header.iovlen) {
        Ok(iovecs) => iovecs,
        Err(error) => return error.to_errno(),
    };
    let total = iovecs.iter().map(|(_, length)| *length).sum::<usize>();
    let socket = match socket_inode(fd) {
        Ok(socket) => socket,
        Err(error) => return error.to_errno(),
    };
    let mut received = match receive_into(&socket, total, flags) {
        Ok(received) => received,
        Err(error) => return error.to_errno(),
    };
    let data_truncated = received.truncated;
    let capacity = usize::try_from(header.controllen).unwrap_or(0);
    let (mut control, mut control_truncated) = match install_rights(
        &mut received,
        header.control,
        capacity,
        flags & MSG_CMSG_CLOEXEC != 0,
    ) {
        Ok(result) => result,
        Err(error) => return error.to_errno(),
    };
    if socket.passcred() {
        if let Some(credentials) = received.credentials {
            if header.control != 0 && capacity.saturating_sub(control.len()) >= CMSG_HEADER + 12 {
                control.extend_from_slice(&((CMSG_HEADER + 12) as u64).to_le_bytes());
                control.extend_from_slice(&unix::SOL_SOCKET.to_le_bytes());
                control.extend_from_slice(&unix::SCM_CREDENTIALS.to_le_bytes());
                control.extend_from_slice(&credentials.encode());
                pad_control(&mut control);
            } else {
                control_truncated = true;
            }
        }
    }
    if let Err(error) = write_payload(&iovecs, &received.bytes) {
        return error.to_errno();
    }
    if header.name != 0 {
        let source = received
            .source
            .clone()
            .or_else(|| socket.peer_address())
            .unwrap_or(unix::UnixAddress::Unnamed);
        if header.namelen != 0 {
            let encoded = source.encode();
            let copy = (header.namelen as usize).min(encoded.len());
            if let Err(error) = uaccess::write(header.name, &encoded[..copy]) {
                return error.to_errno();
            }
        }
        if let Err(error) = uaccess::write_value(message + 8, &(source.encoded_len() as u32)) {
            return error.to_errno();
        }
    }
    if header.control != 0 && !control.is_empty() {
        if let Err(error) = uaccess::write(header.control, &control) {
            return error.to_errno();
        }
    }
    if let Err(error) = uaccess::write_value(message + 40, &(control.len() as u64)) {
        return error.to_errno();
    }
    let mut message_flags = 0i32;
    if control_truncated {
        message_flags |= MSG_CTRUNC;
    }
    if data_truncated && socket.kind() != unix::SocketKind::Stream {
        message_flags |= MSG_TRUNC as i32;
    }
    if socket.kind() == unix::SocketKind::Seqpacket && !received.eof {
        message_flags |= MSG_EOR as i32;
    }
    if let Err(error) = uaccess::write_value(message + 48, &message_flags) {
        return error.to_errno();
    }
    if data_truncated && socket.kind() != unix::SocketKind::Stream && flags & MSG_TRUNC != 0 {
        return received.full_len as i64;
    }
    received.bytes.len() as i64
}

pub fn recvfrom_netlink(
    socket: &Arc<netlink::NetlinkSocket>,
    buffer: u64,
    capacity: u64,
    flags: u64,
    source: u64,
    source_length: u64,
) -> i64 {
    match netlink_receive(
        socket,
        buffer,
        capacity as usize,
        flags,
        source,
        source_length,
    ) {
        Ok((copied, total)) => {
            if flags & MSG_TRUNC != 0 {
                total as i64
            } else {
                copied as i64
            }
        }
        Err(error) => error.to_errno(),
    }
}

pub fn recvfrom(
    fd: u64,
    buffer: u64,
    length: u64,
    flags: u64,
    address: u64,
    address_len: u64,
) -> i64 {
    if let Some(socket) = netlink::instance(fd) {
        return recvfrom_netlink(&socket, buffer, length, flags, address, address_len);
    }
    let allowed = MSG_PEEK | MSG_DONTWAIT | MSG_TRUNC | MSG_WAITALL;
    if flags & !allowed != 0 {
        return -abi::EINVAL;
    }
    if length > MAX_IOV_TOTAL as u64 {
        return -abi::EINVAL;
    }
    let socket = match socket_inode(fd) {
        Ok(socket) => socket,
        Err(error) => return error.to_errno(),
    };
    let received = match receive_into(&socket, length as usize, flags) {
        Ok(received) => received,
        Err(error) => return error.to_errno(),
    };
    if let Err(error) = uaccess::write(buffer, &received.bytes) {
        return error.to_errno();
    }
    if address != 0 {
        let source = received
            .source
            .clone()
            .or_else(|| socket.peer_address())
            .unwrap_or(unix::UnixAddress::Unnamed);
        if let Err(error) = write_sockaddr(address, address_len, &source) {
            return error.to_errno();
        }
    }
    if received.truncated && flags & MSG_TRUNC != 0 {
        return received.full_len as i64;
    }
    received.bytes.len() as i64
}

fn read_optlen(pointer: u64) -> Result<usize, VfsError> {
    if pointer == 0 {
        return Err(VfsError::Fault);
    }
    Ok(uaccess::read_value::<u32>(pointer)? as usize)
}

fn write_option(value: &[u8], pointer: u64, length_pointer: u64) -> Result<(), VfsError> {
    if pointer == 0 || length_pointer == 0 {
        return Err(VfsError::Fault);
    }
    let capacity = read_optlen(length_pointer)?;
    let copy = capacity.min(value.len());
    uaccess::write(pointer, &value[..copy])?;
    uaccess::write_value(length_pointer, &(value.len() as u32))?;
    Ok(())
}

pub fn getsockopt(fd: u64, level: u64, name: u64, value: u64, length: u64) -> i64 {
    if netlink::instance(fd).is_some() {
        let answer: Option<u32> = match (level, name) {
            (1, 3) => Some(3),                                       // SO_TYPE: SOCK_RAW
            (1, 39) => Some(netlink::AF_NETLINK as u32),             // SO_DOMAIN
            (1, 38) => Some(netlink::NETLINK_KOBJECT_UEVENT as u32), // SO_PROTOCOL
            (1, 4) => Some(0),                                       // SO_ERROR
            (1, 7 | 8) => Some(212_992),                             // SO_SNDBUF, SO_RCVBUF
            _ => None,
        };
        let Some(answer) = answer else {
            return -abi::ENOPROTOOPT;
        };
        let result =
            uaccess::write_value(value, &answer).and_then(|()| uaccess::write_value(length, &4u32));
        return match result {
            Ok(()) => 0,
            Err(error) => error.to_errno(),
        };
    }
    let socket = match socket_inode(fd) {
        Ok(socket) => socket,
        Err(error) => return error.to_errno(),
    };
    if level as i32 != unix::SOL_SOCKET {
        return -abi::ENOPROTOOPT;
    }
    let (sndbuf, rcvbuf) = socket.buffers();
    let (bytes, result) = match name as i32 {
        unix::SO_TYPE => (socket.kind().raw().to_le_bytes().to_vec(), Ok(())),
        unix::SO_ERROR => (socket.take_error().to_le_bytes().to_vec(), Ok(())),
        unix::SO_SNDBUF => ((sndbuf as i32).to_le_bytes().to_vec(), Ok(())),
        unix::SO_RCVBUF => ((rcvbuf as i32).to_le_bytes().to_vec(), Ok(())),
        unix::SO_PASSCRED => ((socket.passcred() as i32).to_le_bytes().to_vec(), Ok(())),
        unix::SO_PEERCRED => match socket.peer_credentials() {
            Some(credentials) => (credentials.encode().to_vec(), Ok(())),
            None => (Vec::new(), Err(-abi::ENOTCONN)),
        },
        unix::SO_ACCEPTCONN => (
            (socket.is_listening() as i32).to_le_bytes().to_vec(),
            Ok(()),
        ),
        unix::SO_PROTOCOL => (0i32.to_le_bytes().to_vec(), Ok(())),
        unix::SO_DOMAIN => ((unix::AF_UNIX as i32).to_le_bytes().to_vec(), Ok(())),
        _ => (Vec::new(), Err(-abi::ENOPROTOOPT)),
    };
    if let Err(error) = result {
        return error;
    }
    match write_option(&bytes, value, length) {
        Ok(()) => 0,
        Err(error) => error.to_errno(),
    }
}

// ---- epoll / signalfd / timerfd -------------------------------------------

fn anon_fd(
    inode: Arc<dyn Inode>,
    name: alloc::string::String,
    cloexec: bool,
    nonblock: bool,
) -> i64 {
    let flags = abi::O_RDWR | if nonblock { O_NONBLOCK } else { 0 };
    let file = Arc::new(OpenFile::new(inode, name, flags));
    match alloc_file(file, cloexec) {
        Ok(fd) => fd as i64,
        Err(error) => error.to_errno(),
    }
}

pub fn memfd_create(name: u64, flags: u32) -> i64 {
    if flags & !(memfd::MFD_CLOEXEC | memfd::MFD_ALLOW_SEALING) != 0 {
        return -abi::EINVAL;
    }
    let name = match uaccess::string(name, 250) {
        Ok(name) => name,
        Err(error) => return error.to_errno(),
    };
    let (memfd, cloexec) = match memfd::MemFd::create(name, flags) {
        Ok(memfd) => memfd,
        Err(error) => return error.to_errno(),
    };
    let path = memfd.file_name();
    let file = Arc::new(OpenFile::new(memfd, path, abi::O_RDWR));
    match alloc_file(file, cloexec) {
        Ok(fd) => fd as i64,
        Err(error) => error.to_errno(),
    }
}

pub fn epoll_create1(flags: u32) -> i64 {
    let instance = match epoll::create(flags) {
        Ok(instance) => instance,
        Err(error) => return error.to_errno(),
    };
    anon_fd(
        instance,
        String::from("anon_inode:[eventpoll]"),
        flags & abi::O_CLOEXEC != 0,
        false,
    )
}

pub fn epoll_create(size: i32) -> i64 {
    if size <= 0 {
        return -abi::EINVAL;
    }
    epoll_create1(0)
}

pub fn epoll_ctl(epfd: u64, op: u32, fd: i32, event: u64) -> i64 {
    let instance = match epoll::instance(epfd) {
        Ok(instance) => instance,
        Err(error) => return error.to_errno(),
    };
    match instance.control(op, fd, event) {
        Ok(()) => 0,
        Err(error) => error.to_errno(),
    }
}

pub fn epoll_wait(epfd: u64, events: u64, maxevents: u32, timeout: i64) -> i64 {
    let instance = match epoll::instance(epfd) {
        Ok(instance) => instance,
        Err(error) => return error.to_errno(),
    };
    instance.wait(events, maxevents, timeout)
}

pub fn epoll_pwait(
    epfd: u64,
    events: u64,
    maxevents: u32,
    timeout: i64,
    sigmask: u64,
    sigsetsize: u64,
) -> i64 {
    if sigmask != 0 {
        if sigsetsize != 8 {
            return -abi::EINVAL;
        }
        match uaccess::read_value::<u64>(sigmask) {
            Ok(mask) => crate::signal::install_wait_mask(mask),
            Err(error) => return error.to_errno(),
        }
    }
    epoll_wait(epfd, events, maxevents, timeout)
}

pub fn epoll_pwait2(
    epfd: u64,
    events: u64,
    maxevents: u32,
    timeout: u64,
    sigmask: u64,
    sigsetsize: u64,
) -> i64 {
    let millis = if timeout == 0 {
        -1
    } else {
        let value: [i64; 2] = match uaccess::read_value(timeout) {
            Ok(value) => value,
            Err(error) => return error.to_errno(),
        };
        if value[0] < 0 || !(0..1_000_000_000).contains(&value[1]) {
            return -abi::EINVAL;
        }
        value[0]
            .saturating_mul(1000)
            .saturating_add((value[1] as u64).div_ceil(1_000_000) as i64)
    };
    epoll_pwait(epfd, events, maxevents, millis, sigmask, sigsetsize)
}

pub fn inotify_init1(flags: u32) -> i64 {
    const IN_CLOEXEC: u32 = 0o2000000;
    const IN_NONBLOCK: u32 = 0o4000;
    if flags & !(IN_CLOEXEC | IN_NONBLOCK) != 0 {
        return -abi::EINVAL;
    }
    let instance = inotify::Inotify::create(flags & IN_NONBLOCK != 0);
    let file = Arc::new(OpenFile::new(
        instance,
        alloc::string::String::from("anon_inode:inotify"),
        abi::O_RDONLY,
    ));
    match alloc_file(file, flags & IN_CLOEXEC != 0) {
        Ok(fd) => fd as i64,
        Err(error) => error.to_errno(),
    }
}

pub fn inotify_add_watch(fd: u64, path: u64, mask: u32) -> i64 {
    let instance = match inotify::instance(fd) {
        Ok(instance) => instance,
        Err(error) => return error.to_errno(),
    };
    let result = uaccess::string(path, 4096).and_then(|text| {
        let full = vfs::resolve_at_path(&vfs::current_fd_table().lock(), abi::AT_FDCWD, &text)?;
        instance.add_watch(&full, mask)
    });
    match result {
        Ok(wd) => i64::from(wd),
        Err(error) => error.to_errno(),
    }
}

pub fn inotify_rm_watch(fd: u64, wd: i32) -> i64 {
    match inotify::instance(fd).and_then(|instance| instance.remove_watch(wd)) {
        Ok(()) => 0,
        Err(error) => error.to_errno(),
    }
}

pub fn signalfd4(fd: i64, mask: u64, sigsetsize: u64, flags: u32) -> i64 {
    if sigsetsize != 8 {
        return -abi::EINVAL;
    }
    if flags & !(abi::O_CLOEXEC | O_NONBLOCK) != 0 {
        return -abi::EINVAL;
    }
    let mask = match uaccess::read_value::<u64>(mask) {
        Ok(mask) => mask,
        Err(error) => return error.to_errno(),
    };
    let nonblock = flags & O_NONBLOCK != 0;
    if fd == -1 {
        let inode = signalfd::SignalFd::new();
        inode.set_mask(mask);
        inode.set_nonblock(nonblock);
        return anon_fd(
            inode,
            String::from("anon_inode:[signalfd]"),
            flags & abi::O_CLOEXEC != 0,
            nonblock,
        );
    }
    if fd < 0 {
        return -abi::EBADF;
    }
    let instance = match signalfd::instance(fd as u64) {
        Ok(instance) => instance,
        Err(error) => return error.to_errno(),
    };
    instance.set_mask(mask);
    fd
}

pub fn signalfd(fd: i64, mask: u64, sigsetsize: u64) -> i64 {
    signalfd4(fd, mask, sigsetsize, 0)
}

pub fn timerfd_create(clockid: u32, flags: u32) -> i64 {
    const CLOCK_MONOTONIC: u32 = 1;
    const CLOCK_BOOTTIME: u32 = 7;
    if flags & !(abi::O_CLOEXEC | O_NONBLOCK) != 0 {
        return -abi::EINVAL;
    }
    if !matches!(clockid, CLOCK_MONOTONIC | CLOCK_BOOTTIME) {
        // CLOCK_REALTIME needs a wall-clock source that does not exist yet.
        return -abi::EINVAL;
    }
    let nonblock = flags & O_NONBLOCK != 0;
    anon_fd(
        timerfd::TimerFd::create(clockid, nonblock),
        String::from("anon_inode:[timerfd]"),
        flags & abi::O_CLOEXEC != 0,
        nonblock,
    )
}

pub fn timerfd_settime(fd: u64, flags: u32, new_value: u64, old_value: u64) -> i64 {
    let instance = match timerfd::instance(fd) {
        Ok(instance) => instance,
        Err(error) => return error.to_errno(),
    };
    let spec = match uaccess::read_value::<[u64; 4]>(new_value) {
        Ok(spec) => spec,
        Err(error) => return error.to_errno(),
    };
    let old = match instance.settime(flags, &spec) {
        Ok(old) => old,
        Err(error) => return error.to_errno(),
    };
    // A task sleeping on this timer computed its wakeup from the old deadline.
    wake_pollers();
    if old_value != 0
        && let Err(error) = uaccess::write_value(old_value, &old)
    {
        return error.to_errno();
    }
    0
}

pub fn timerfd_gettime(fd: u64, old_value: u64) -> i64 {
    let instance = match timerfd::instance(fd) {
        Ok(instance) => instance,
        Err(error) => return error.to_errno(),
    };
    if old_value == 0 {
        return -abi::EFAULT;
    }
    match uaccess::write_value(old_value, &instance.gettime()) {
        Ok(()) => 0,
        Err(error) => error.to_errno(),
    }
}

pub fn setsockopt(fd: u64, level: u64, name: u64, value: u64, length: u64) -> i64 {
    if netlink::instance(fd).is_some() {
        return match netlink::setsockopt(level, name) {
            Ok(()) => 0,
            Err(error) => error.to_errno(),
        };
    }
    let socket = match socket_inode(fd) {
        Ok(socket) => socket,
        Err(error) => return error.to_errno(),
    };
    if level as i32 != unix::SOL_SOCKET {
        return -abi::ENOPROTOOPT;
    }
    let length = length as usize;
    if length < 4 || value == 0 {
        return -abi::EINVAL;
    }
    let setting = match uaccess::read_value::<i32>(value) {
        Ok(setting) => setting,
        Err(error) => return error.to_errno(),
    };
    match name as i32 {
        unix::SO_PASSCRED => {
            if socket.passcred() != (setting != 0) {
                if let Err(error) = socket.set_passcred(setting != 0) {
                    return error.to_errno();
                }
            }
            if setting != 0 && socket.local_address().is_unnamed() {
                // SO_PASSCRED on an unbound socket autobinds an abstract name.
                if let Err(error) = socket.bind(unix::UnixAddress::Unnamed) {
                    return error.to_errno();
                }
            }
            0
        }
        unix::SO_SNDBUF => {
            socket.set_buffer(Some(setting.max(0) as usize), None);
            0
        }
        unix::SO_RCVBUF => {
            socket.set_buffer(None, Some(setting.max(0) as usize));
            0
        }
        _ => -abi::ENOPROTOOPT,
    }
}
