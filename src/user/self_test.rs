/*
 *       src/user/self_test.rs
 *       Real page-table permissions, copies and rollback verification
 *       2026/10/24 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 */
use super::{UserAddressSpace, elf, elf_format};
pub fn run() {
    let free = crate::mm::pmm::stats().free;
    {
        let mut s = UserAddressSpace::new().unwrap();
        assert!(
            s.map_zeroed(UserAddressSpace::user_limit(), 4096, 3)
                .is_err()
        );
        s.map_zeroed(0x10000, 8192, 3).unwrap();
        s.write_bytes(0x10fff, b"cross-page").unwrap();
        let mut bytes = [0; 10];
        s.read_bytes(0x10fff, &mut bytes).unwrap();
        assert_eq!(&bytes, b"cross-page");
        assert!(s.validate(0x11fff, 2, false).is_err());
        assert!(s.validate(u64::MAX, 2, false).is_err());
        s.protect(0x11000, 4096, 1).unwrap();
        assert!(s.write_bytes(0x10fff, b"no partial write").is_err());
        let mut first = [0];
        s.read_bytes(0x10fff, &mut first).unwrap();
        assert_eq!(first[0], b'c');
        s.protect(0x11000, 4096, 0).unwrap();
        assert!(s.read_bytes(0x11000, &mut first).is_err());
        s.protect(0x11000, 4096, 3).unwrap();
        s.unmap(0x11000, 4096).unwrap();
        assert!(s.read_bytes(0x11000, &mut first).is_err());
        assert!(s.map_zeroed(0x10000, 4096, 3).is_err());
        s.unmap(0x10000, 4096).unwrap();
        let data = super::test_binaries::TEST_USER_ELF;
        let loaded = elf::load_elf(&mut s, data).unwrap();
        assert!(s.validate(loaded.entry, 1, false).is_ok());
        let mut bad = data.to_vec();
        bad[54..56].fill(0);
        let before = s.resident_pages();
        assert!(elf::load_elf(&mut s, &bad).is_err());
        assert_eq!(before, s.resident_pages());

        // Construct an ELF with PT_LOAD (text + data), PT_GNU_RELRO, and PT_TLS
        let mut custom = alloc::vec![0u8; 12288];
        custom[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
        custom[16..18].copy_from_slice(&2u16.to_le_bytes()); // ET_EXEC
        let machine = u16::from_le_bytes([data[18], data[19]]);
        custom[18..20].copy_from_slice(&machine.to_le_bytes());
        custom[20..24].copy_from_slice(&1u32.to_le_bytes());
        custom[24..32].copy_from_slice(&0x3000_0000u64.to_le_bytes()); // entry
        custom[32..40].copy_from_slice(&64u64.to_le_bytes()); // phoff
        custom[52..54].copy_from_slice(&64u16.to_le_bytes());
        custom[54..56].copy_from_slice(&56u16.to_le_bytes());
        custom[56..58].copy_from_slice(&4u16.to_le_bytes()); // 4 phdrs
        // Seg 0: PT_LOAD RX at 0x3000_0000 (offset 4096, size 4096)
        let mut ph = 64usize;
        custom[ph..ph + 4].copy_from_slice(&elf_format::PT_LOAD.to_le_bytes());
        custom[ph + 4..ph + 8].copy_from_slice(&5u32.to_le_bytes());
        custom[ph + 8..ph + 16].copy_from_slice(&4096u64.to_le_bytes());
        custom[ph + 16..ph + 24].copy_from_slice(&0x3000_0000u64.to_le_bytes());
        custom[ph + 32..ph + 40].copy_from_slice(&4096u64.to_le_bytes());
        custom[ph + 40..ph + 48].copy_from_slice(&4096u64.to_le_bytes());
        custom[ph + 48..ph + 56].copy_from_slice(&4096u64.to_le_bytes());
        // Seg 1: PT_LOAD RW at 0x3000_1000 (offset 8192, filesz 4096, memsz 8192)
        ph += 56;
        custom[ph..ph + 4].copy_from_slice(&elf_format::PT_LOAD.to_le_bytes());
        custom[ph + 4..ph + 8].copy_from_slice(&6u32.to_le_bytes());
        custom[ph + 8..ph + 16].copy_from_slice(&8192u64.to_le_bytes());
        custom[ph + 16..ph + 24].copy_from_slice(&0x3000_1000u64.to_le_bytes());
        custom[ph + 32..ph + 40].copy_from_slice(&4096u64.to_le_bytes());
        custom[ph + 40..ph + 48].copy_from_slice(&8192u64.to_le_bytes());
        custom[ph + 48..ph + 56].copy_from_slice(&4096u64.to_le_bytes());
        // Seg 2: PT_GNU_RELRO covering 0x3000_1000..0x3000_2000
        ph += 56;
        custom[ph..ph + 4].copy_from_slice(&elf_format::PT_GNU_RELRO.to_le_bytes());
        custom[ph + 4..ph + 8].copy_from_slice(&4u32.to_le_bytes());
        custom[ph + 8..ph + 16].copy_from_slice(&8192u64.to_le_bytes());
        custom[ph + 16..ph + 24].copy_from_slice(&0x3000_1000u64.to_le_bytes());
        custom[ph + 32..ph + 40].copy_from_slice(&4096u64.to_le_bytes());
        custom[ph + 40..ph + 48].copy_from_slice(&4096u64.to_le_bytes());
        custom[ph + 48..ph + 56].copy_from_slice(&1u64.to_le_bytes());
        // Seg 3: PT_TLS inside Seg 1 (offset 8192, vaddr 0x3000_1000, filesz 8, memsz 16, align 8)
        ph += 56;
        custom[ph..ph + 4].copy_from_slice(&elf_format::PT_TLS.to_le_bytes());
        custom[ph + 4..ph + 8].copy_from_slice(&4u32.to_le_bytes());
        custom[ph + 8..ph + 16].copy_from_slice(&8192u64.to_le_bytes());
        custom[ph + 16..ph + 24].copy_from_slice(&0x3000_1000u64.to_le_bytes());
        custom[ph + 32..ph + 40].copy_from_slice(&8u64.to_le_bytes());
        custom[ph + 40..ph + 48].copy_from_slice(&16u64.to_le_bytes());
        custom[ph + 48..ph + 56].copy_from_slice(&8u64.to_le_bytes());
        custom[8192..8200].copy_from_slice(&0xdead_beef_cafe_babeu64.to_le_bytes());

        let mut s2 = UserAddressSpace::new().unwrap();
        let loaded2 = elf::load_elf(&mut s2, &custom).unwrap();
        assert_ne!(loaded2.tls_tp, 0);
        // RELRO page 0x3000_1000 must now be read-only and blocked from write escalation
        assert!(s2.validate(0x3000_1000, 8, false).is_ok());
        assert!(s2.validate(0x3000_1000, 8, true).is_err());
        assert!(s2.protect(0x3000_1000, 4096, 3).is_err());
        // Non-RELRO BSS page 0x3000_2000 remains writable
        assert!(s2.validate(0x3000_2000, 8, true).is_ok());
    }
    {
        // CoW fork: both sides start on one physical page, each side's first
        // write gets a private copy, and dropping either side returns the
        // retained frame to the PMM.
        let free_before = crate::mm::pmm::stats().free;
        let mut parent = UserAddressSpace::new().unwrap();
        parent.map_zeroed(0x7000_0000, 8192, 3).unwrap();
        parent.write_bytes(0x7000_0000, b"parent-cow").unwrap();
        let mut child = parent.fork_copy().unwrap();
        let mut bytes = [0u8; 10];
        child.read_bytes(0x7000_0000, &mut bytes).unwrap();
        assert_eq!(&bytes, b"parent-cow");
        parent.write_bytes(0x7000_0000, b"parent-new").unwrap();
        child.read_bytes(0x7000_0000, &mut bytes).unwrap();
        assert_eq!(&bytes, b"parent-cow");
        parent.read_bytes(0x7000_0000, &mut bytes).unwrap();
        assert_eq!(&bytes, b"parent-new");
        child.write_bytes(0x7000_0000, b"child-new").unwrap();
        child.read_bytes(0x7000_0000, &mut bytes[..9]).unwrap();
        assert_eq!(&bytes[..9], b"child-new");
        parent.read_bytes(0x7000_0000, &mut bytes).unwrap();
        assert_eq!(&bytes, b"parent-new");
        drop(child);
        parent.write_bytes(0x7000_0000, b"parent-end").unwrap();
        parent.read_bytes(0x7000_0000, &mut bytes).unwrap();
        assert_eq!(&bytes, b"parent-end");
        drop(parent);
        assert_eq!(
            crate::mm::pmm::stats().free,
            free_before,
            "CoW fork leaked physical frames"
        );
    }
    assert_eq!(
        crate::mm::pmm::stats().free,
        free,
        "user address space leaks physical frames"
    );
    crate::verbose::log(format_args!("user VM/ELF boundary self-test passed"));
}

pub fn syscalls() {
    use crate::{
        sync::SpinLock,
        syscall::{dispatch, numbers::*},
        vfs::{self, abi},
    };
    use alloc::sync::Arc;
    let task = crate::sched::current().unwrap();
    let old = task.address_space();
    let scratch = Arc::new(SpinLock::new(UserAddressSpace::new().unwrap()));
    scratch.lock().map_zeroed(0x10000, 16384, 3).unwrap();
    task.lock().set_address_space(scratch.clone());
    let read_i32 = |address: u64| -> i32 {
        let mut bytes = [0u8; 4];
        scratch.lock().read_bytes(address, &mut bytes).unwrap();
        i32::from_le_bytes(bytes)
    };
    let read_u32 = |address: u64| -> u32 {
        let mut bytes = [0u8; 4];
        scratch.lock().read_bytes(address, &mut bytes).unwrap();
        u32::from_le_bytes(bytes)
    };
    assert_eq!(
        dispatch(SYS_WRITE, 1, UserAddressSpace::user_limit(), 8, 0, 0, 0),
        -abi::EFAULT
    );
    let address = dispatch(SYS_MMAP, 0, 8192, 3, 0x22, u64::MAX, 0);
    assert!(address > 0);
    // Anonymous mmap is a lazy reservation.  A user store would fault the
    // pages in; the kernel-side writer must do the same through the normal
    // fault path before touching both pages of the cross-page range.
    crate::user::uaccess::prefault(address as u64, 8192, true).unwrap();
    scratch
        .lock()
        .write_bytes(address as u64 + 4095, b"mapped cross page")
        .unwrap();
    assert_eq!(dispatch(SYS_MPROTECT, address as u64, 8192, 0, 0, 0, 0), 0);
    assert_eq!(
        dispatch(SYS_WRITE, 1, address as u64, 1, 0, 0, 0),
        -abi::EFAULT
    );
    assert_eq!(dispatch(SYS_MPROTECT, address as u64, 8192, 3, 0, 0, 0), 0);
    assert_eq!(dispatch(SYS_MUNMAP, address as u64, 8192, 0, 0, 0, 0), 0);
    assert!(scratch.lock().validate(address as u64, 1, false).is_err());
    let base = dispatch(SYS_BRK, 0, 0, 0, 0, 0, 0) as u64;
    assert_eq!(
        dispatch(SYS_BRK, base + 8192, 0, 0, 0, 0, 0) as u64,
        base + 8192
    );
    assert!(scratch.lock().validate(base, 8192, true).is_ok());
    assert_eq!(dispatch(SYS_BRK, base, 0, 0, 0, 0, 0) as u64, base);

    // Verify MAP_FIXED failure rollback preserves existing mapping
    let keep = dispatch(SYS_MMAP, 0, 4096, 3, 0x22, u64::MAX, 0) as u64;
    assert!(keep >= 0x4000_0000);
    crate::user::uaccess::prefault(keep, 4096, true).unwrap();
    scratch.lock().write_bytes(keep, b"keep-alive").unwrap();
    // MAP_FIXED with invalid fd (-1) and non-anonymous flags (0x12) must fail before unmapping `keep`
    assert_eq!(
        dispatch(SYS_MMAP, keep, 4096, 3, 0x12, u64::MAX, 0),
        -abi::EBADF
    );
    let mut check_keep = [0u8; 10];
    scratch.lock().read_bytes(keep, &mut check_keep).unwrap();
    assert_eq!(&check_keep, b"keep-alive");
    assert_eq!(dispatch(SYS_MUNMAP, keep, 4096, 0, 0, 0, 0), 0);

    // Verify pipe2 flags, O_NONBLOCK, poll, and readv/writev edge cases
    assert_eq!(
        dispatch(SYS_PIPE2, 0x10000, 0xffff, 0, 0, 0, 0),
        -abi::EINVAL
    );
    assert_eq!(
        dispatch(SYS_PIPE2, 0x10000, abi::O_NONBLOCK as u64, 0, 0, 0, 0),
        0
    );
    let mut fds = [0i32; 2];
    scratch
        .lock()
        .read_bytes(0x10000, unsafe {
            core::slice::from_raw_parts_mut(fds.as_mut_ptr() as *mut u8, 8)
        })
        .unwrap();
    assert_eq!(
        dispatch(SYS_READ, fds[0] as u64, 0x10100, 4, 0, 0, 0),
        -abi::EAGAIN
    );
    assert_eq!(dispatch(SYS_READV, fds[0] as u64, 0, 0, 0, 0, 0), 0);
    assert_eq!(
        dispatch(SYS_READV, fds[0] as u64, 0, 1, 0, 0, 0),
        -abi::EFAULT
    );
    assert_eq!(dispatch(SYS_CLOSE, fds[0] as u64, 0, 0, 0, 0, 0), 0);
    assert_eq!(dispatch(SYS_CLOSE, fds[1] as u64, 0, 0, 0, 0, 0), 0);

    // eventfd2: counter, semaphore mode, nonblocking EAGAIN and poll readiness
    assert_eq!(
        dispatch(SYS_EVENTFD2, 0, 0xffff_ffff, 0, 0, 0, 0),
        -abi::EINVAL
    );
    let efd = dispatch(SYS_EVENTFD2, 0, 0, 0, 0, 0, 0);
    assert!(efd >= 0);
    scratch
        .lock()
        .write_bytes(0x10200, &5u64.to_le_bytes())
        .unwrap();
    assert_eq!(dispatch(SYS_WRITE, efd as u64, 0x10200, 8, 0, 0, 0), 8);
    assert_eq!(dispatch(SYS_READ, efd as u64, 0x10208, 8, 0, 0, 0), 8);
    let mut event_value = [0u8; 8];
    scratch
        .lock()
        .read_bytes(0x10208, &mut event_value)
        .unwrap();
    assert_eq!(u64::from_le_bytes(event_value), 5);
    #[cfg(target_arch = "x86_64")]
    {
        scratch
            .lock()
            .write_bytes(0x10200, &3u64.to_le_bytes())
            .unwrap();
        assert_eq!(dispatch(SYS_WRITE, efd as u64, 0x10200, 8, 0, 0, 0), 8);
        scratch
            .lock()
            .write_bytes(0x10210, &(efd as i32).to_le_bytes())
            .unwrap();
        scratch
            .lock()
            .write_bytes(0x10214, &1u16.to_le_bytes())
            .unwrap();
        assert_eq!(dispatch(SYS_POLL, 0x10210, 1, 1000, 0, 0, 0), 1);
    }
    assert_eq!(dispatch(SYS_CLOSE, efd as u64, 0, 0, 0, 0, 0), 0);

    let empty = dispatch(SYS_EVENTFD2, 0, abi::O_NONBLOCK as u64, 0, 0, 0, 0);
    assert!(empty >= 0);
    assert_eq!(
        dispatch(SYS_READ, empty as u64, 0x10220, 8, 0, 0, 0),
        -abi::EAGAIN
    );
    assert_eq!(dispatch(SYS_CLOSE, empty as u64, 0, 0, 0, 0, 0), 0);

    let sem = dispatch(SYS_EVENTFD2, 3, 1, 0, 0, 0, 0);
    assert!(sem >= 0);
    assert_eq!(dispatch(SYS_READ, sem as u64, 0x10228, 8, 0, 0, 0), 8);
    scratch
        .lock()
        .read_bytes(0x10228, &mut event_value)
        .unwrap();
    assert_eq!(u64::from_le_bytes(event_value), 1);
    assert_eq!(dispatch(SYS_CLOSE, sem as u64, 0, 0, 0, 0, 0), 0);

    // AF_UNIX socketpair: byte stream, SCM_RIGHTS, shutdown/EOF
    assert_eq!(dispatch(SYS_SOCKETPAIR, 1, 1, 0, 0x10300, 0, 0), 0);
    let mut pair = [0i32; 2];
    scratch
        .lock()
        .read_bytes(0x10300, unsafe {
            core::slice::from_raw_parts_mut(pair.as_mut_ptr() as *mut u8, 8)
        })
        .unwrap();
    scratch.lock().write_bytes(0x10310, b"hello").unwrap();
    assert_eq!(dispatch(SYS_WRITE, pair[0] as u64, 0x10310, 5, 0, 0, 0), 5);
    assert_eq!(dispatch(SYS_READ, pair[1] as u64, 0x10320, 5, 0, 0, 0), 5);
    let mut stream = [0u8; 5];
    scratch.lock().read_bytes(0x10320, &mut stream).unwrap();
    assert_eq!(&stream, b"hello");

    let carried = dispatch(SYS_EVENTFD2, 7, 0, 0, 0, 0, 0);
    assert!(carried >= 0);
    // sendmsg: one byte + SCM_RIGHTS(fd=carried)
    let mut send_iovec = [0u8; 16];
    send_iovec[0..8].copy_from_slice(&0x10310u64.to_le_bytes());
    send_iovec[8..16].copy_from_slice(&1u64.to_le_bytes());
    scratch.lock().write_bytes(0x10380, &send_iovec).unwrap();
    let mut cmsg = [0u8; 24];
    cmsg[0..8].copy_from_slice(&20u64.to_le_bytes());
    cmsg[8..12].copy_from_slice(&1i32.to_le_bytes());
    cmsg[12..16].copy_from_slice(&1i32.to_le_bytes());
    cmsg[16..20].copy_from_slice(&(carried as i32).to_le_bytes());
    scratch.lock().write_bytes(0x10390, &cmsg).unwrap();
    let mut send_header = [0u8; 56];
    send_header[16..24].copy_from_slice(&0x10380u64.to_le_bytes());
    send_header[24..32].copy_from_slice(&1u64.to_le_bytes());
    send_header[32..40].copy_from_slice(&0x10390u64.to_le_bytes());
    send_header[40..48].copy_from_slice(&20u64.to_le_bytes());
    scratch.lock().write_bytes(0x10340, &send_header).unwrap();
    assert_eq!(
        dispatch(SYS_SENDMSG, pair[0] as u64, 0x10340, 0, 0, 0, 0),
        1
    );

    let mut recv_iovec = [0u8; 16];
    recv_iovec[0..8].copy_from_slice(&0x10480u64.to_le_bytes());
    recv_iovec[8..16].copy_from_slice(&1u64.to_le_bytes());
    scratch.lock().write_bytes(0x103F0, &recv_iovec).unwrap();
    let mut recv_header = [0u8; 56];
    recv_header[16..24].copy_from_slice(&0x103F0u64.to_le_bytes());
    recv_header[24..32].copy_from_slice(&1u64.to_le_bytes());
    recv_header[32..40].copy_from_slice(&0x103B0u64.to_le_bytes());
    recv_header[40..48].copy_from_slice(&64u64.to_le_bytes());
    scratch.lock().write_bytes(0x10400, &recv_header).unwrap();
    assert_eq!(
        dispatch(SYS_RECVMSG, pair[1] as u64, 0x10400, 0, 0, 0, 0),
        1
    );
    let mut received_byte = [0u8; 1];
    scratch
        .lock()
        .read_bytes(0x10480, &mut received_byte)
        .unwrap();
    assert_eq!(received_byte[0], b'h');
    let mut received_cmsg = [0u8; 20];
    scratch
        .lock()
        .read_bytes(0x103B0, &mut received_cmsg)
        .unwrap();
    assert_eq!(
        u64::from_le_bytes(received_cmsg[0..8].try_into().unwrap()),
        20
    );
    let received_fd = i32::from_le_bytes(received_cmsg[16..20].try_into().unwrap());
    assert!(received_fd >= 0);
    scratch
        .lock()
        .write_bytes(0x10450, &9u64.to_le_bytes())
        .unwrap();
    assert_eq!(
        dispatch(SYS_WRITE, received_fd as u64, 0x10450, 8, 0, 0, 0),
        8
    );
    assert_eq!(dispatch(SYS_READ, carried as u64, 0x10458, 8, 0, 0, 0), 8);
    let mut carried_value = [0u8; 8];
    scratch
        .lock()
        .read_bytes(0x10458, &mut carried_value)
        .unwrap();
    assert_eq!(u64::from_le_bytes(carried_value), 16);
    assert_eq!(dispatch(SYS_CLOSE, received_fd as u64, 0, 0, 0, 0, 0), 0);
    assert_eq!(dispatch(SYS_CLOSE, carried as u64, 0, 0, 0, 0, 0), 0);

    assert_eq!(dispatch(SYS_SHUTDOWN, pair[0] as u64, 1, 0, 0, 0, 0), 0);
    assert_eq!(dispatch(SYS_READ, pair[1] as u64, 0x10320, 1, 0, 0, 0), 0);
    assert_eq!(dispatch(SYS_CLOSE, pair[0] as u64, 0, 0, 0, 0, 0), 0);
    assert_eq!(dispatch(SYS_CLOSE, pair[1] as u64, 0, 0, 0, 0, 0), 0);

    // SOCK_DGRAM socketpair preserves message boundaries and MSG_TRUNC length.
    crate::verbose::log(format_args!("ipc selftest: dgram"));
    assert_eq!(dispatch(SYS_SOCKETPAIR, 1, 2, 0, 0x10500, 0, 0), 0);
    let dgram_left = read_i32(0x10500);
    let dgram_right = read_i32(0x10504);
    scratch.lock().write_bytes(0x10510, b"one").unwrap();
    assert_eq!(
        dispatch(SYS_SENDTO, dgram_left as u64, 0x10510, 3, 0, 0, 0),
        3
    );
    scratch.lock().write_bytes(0x10510, b"two!").unwrap();
    assert_eq!(
        dispatch(SYS_SENDTO, dgram_left as u64, 0x10510, 4, 0, 0, 0),
        4
    );
    assert_eq!(
        dispatch(SYS_RECVFROM, dgram_right as u64, 0x10520, 2, 0x20, 0, 0),
        3,
        "MSG_TRUNC must report the full datagram length"
    );
    let mut stream = [0u8; 8];
    scratch
        .lock()
        .read_bytes(0x10520, &mut stream[..2])
        .unwrap();
    assert_eq!(&stream[..2], b"on");
    assert_eq!(
        dispatch(SYS_RECVFROM, dgram_right as u64, 0x10520, 16, 0, 0, 0),
        4
    );
    scratch
        .lock()
        .read_bytes(0x10520, &mut stream[..4])
        .unwrap();
    assert_eq!(&stream[..4], b"two!");
    assert_eq!(dispatch(SYS_CLOSE, dgram_left as u64, 0, 0, 0, 0, 0), 0);
    assert_eq!(dispatch(SYS_CLOSE, dgram_right as u64, 0, 0, 0, 0, 0), 0);

    // Pathname stream socket: bind/listen/connect/accept, getsockname,
    // getpeername and SO_PEERCRED.
    crate::verbose::log(format_args!("ipc selftest: pathname"));
    let path = b"/tmp/afunix-test.sock\0";
    let mut address = [0u8; 32];
    address[0] = 1;
    address[2..2 + path.len()].copy_from_slice(path);
    let address_len = 2 + path.len();
    scratch
        .lock()
        .write_bytes(0x10600, &address[..address_len])
        .unwrap();
    let server = dispatch(SYS_SOCKET, 1, 1 | 0x80000, 0, 0, 0, 0);
    assert!(server >= 0);
    assert_eq!(
        dispatch(
            SYS_BIND,
            server as u64,
            0x10600,
            address_len as u64,
            0,
            0,
            0
        ),
        0
    );
    assert_eq!(dispatch(SYS_LISTEN, server as u64, 4, 0, 0, 0, 0), 0);
    let client = dispatch(SYS_SOCKET, 1, 1, 0, 0, 0, 0);
    assert!(client >= 0);
    assert_eq!(
        dispatch(
            SYS_CONNECT,
            client as u64,
            0x10600,
            address_len as u64,
            0,
            0,
            0
        ),
        0
    );
    scratch
        .lock()
        .write_bytes(0x10770, &110u32.to_le_bytes())
        .unwrap();
    let accepted = dispatch(SYS_ACCEPT4, server as u64, 0x10700, 0x10770, 0x80000, 0, 0);
    assert!(accepted >= 0);
    assert!(read_u32(0x10770) > 2);
    scratch.lock().write_bytes(0x10620, b"path").unwrap();
    assert_eq!(dispatch(SYS_WRITE, client as u64, 0x10620, 4, 0, 0, 0), 4);
    assert_eq!(dispatch(SYS_READ, accepted as u64, 0x10630, 4, 0, 0, 0), 4);
    let mut payload = [0u8; 4];
    scratch.lock().read_bytes(0x10630, &mut payload).unwrap();
    assert_eq!(&payload, b"path");
    scratch
        .lock()
        .write_bytes(0x10770, &110u32.to_le_bytes())
        .unwrap();
    assert_eq!(
        dispatch(SYS_GETPEERNAME, client as u64, 0x10700, 0x10770, 0, 0, 0),
        0
    );
    let mut peer_address = [0u8; 32];
    scratch
        .lock()
        .read_bytes(0x10700, &mut peer_address[..address_len])
        .unwrap();
    assert_eq!(u16::from_le_bytes([peer_address[0], peer_address[1]]), 1);
    assert_eq!(&peer_address[2..2 + path.len()], path);
    scratch
        .lock()
        .write_bytes(0x10780, &12u32.to_le_bytes())
        .unwrap();
    assert_eq!(
        dispatch(SYS_GETSOCKOPT, accepted as u64, 1, 17, 0x10790, 0x10780, 0),
        0
    );
    let mut credentials = [0u8; 12];
    scratch
        .lock()
        .read_bytes(0x10790, &mut credentials)
        .unwrap();
    assert_eq!(
        u32::from_le_bytes(credentials[0..4].try_into().unwrap()),
        task.stats().tgid as u32
    );
    assert_eq!(u32::from_le_bytes(credentials[4..8].try_into().unwrap()), 0);

    // Abstract namespace: same flow without a filesystem path.
    crate::verbose::log(format_args!("ipc selftest: abstract"));
    let abstract_address = [1u8, 0, 0, b'a', b'b', b's'];
    scratch
        .lock()
        .write_bytes(0x10680, &abstract_address)
        .unwrap();
    let abstract_server = dispatch(SYS_SOCKET, 1, 1, 0, 0, 0, 0);
    assert!(abstract_server >= 0);
    assert_eq!(
        dispatch(SYS_BIND, abstract_server as u64, 0x10680, 6, 0, 0, 0),
        0
    );
    assert_eq!(
        dispatch(SYS_LISTEN, abstract_server as u64, 4, 0, 0, 0, 0),
        0
    );
    let abstract_client = dispatch(SYS_SOCKET, 1, 1, 0, 0, 0, 0);
    assert_eq!(
        dispatch(SYS_CONNECT, abstract_client as u64, 0x10680, 6, 0, 0, 0),
        0
    );
    let abstract_accepted = dispatch(SYS_ACCEPT, abstract_server as u64, 0, 0, 0, 0, 0);
    assert!(abstract_accepted >= 0);
    scratch.lock().write_bytes(0x106A0, b"abs").unwrap();
    assert_eq!(
        dispatch(SYS_WRITE, abstract_client as u64, 0x106A0, 3, 0, 0, 0),
        3
    );
    assert_eq!(
        dispatch(SYS_READ, abstract_accepted as u64, 0x106B0, 3, 0, 0, 0),
        3
    );
    scratch
        .lock()
        .read_bytes(0x106B0, &mut payload[..3])
        .unwrap();
    assert_eq!(&payload[..3], b"abs");

    // unlink removes the pathname binding and later connects fail with ENOENT.
    let path_string = b"/tmp/afunix-test.sock\0";
    scratch.lock().write_bytes(0x106C0, path_string).unwrap();
    assert_eq!(
        dispatch(
            SYS_UNLINKAT,
            abi::AT_FDCWD as i64 as u64,
            0x106C0,
            0,
            0,
            0,
            0
        ),
        0
    );
    let late_client = dispatch(SYS_SOCKET, 1, 1, 0, 0, 0, 0);
    assert_eq!(
        dispatch(
            SYS_CONNECT,
            late_client as u64,
            0x10600,
            address_len as u64,
            0,
            0,
            0
        ),
        -abi::ENOENT
    );

    // SCM_RIGHTS with a control buffer too small: MSG_CTRUNC and no fd.
    crate::verbose::log(format_args!("ipc selftest: rights"));
    assert_eq!(dispatch(SYS_SOCKETPAIR, 1, 1, 0, 0x10800, 0, 0), 0);
    let rights_left = read_i32(0x10800);
    let rights_right = read_i32(0x10804);
    let carried = dispatch(SYS_EVENTFD2, 7, 0, 0, 0, 0, 0);
    assert!(carried >= 0);
    scratch.lock().write_bytes(0x10B00, b"R").unwrap();
    let mut send_iovec = [0u8; 16];
    send_iovec[0..8].copy_from_slice(&0x10B00u64.to_le_bytes());
    send_iovec[8..16].copy_from_slice(&1u64.to_le_bytes());
    scratch.lock().write_bytes(0x10940, &send_iovec).unwrap();
    let mut cmsg = [0u8; 20];
    cmsg[0..8].copy_from_slice(&20u64.to_le_bytes());
    cmsg[8..12].copy_from_slice(&1i32.to_le_bytes());
    cmsg[12..16].copy_from_slice(&1i32.to_le_bytes());
    cmsg[16..20].copy_from_slice(&(carried as i32).to_le_bytes());
    scratch.lock().write_bytes(0x10950, &cmsg).unwrap();
    let mut send_header = [0u8; 56];
    send_header[16..24].copy_from_slice(&0x10940u64.to_le_bytes());
    send_header[24..32].copy_from_slice(&1u64.to_le_bytes());
    send_header[32..40].copy_from_slice(&0x10950u64.to_le_bytes());
    send_header[40..48].copy_from_slice(&20u64.to_le_bytes());
    scratch.lock().write_bytes(0x10900, &send_header).unwrap();
    assert_eq!(
        dispatch(SYS_SENDMSG, rights_left as u64, 0x10900, 0, 0, 0, 0),
        1
    );
    let mut recv_iovec = [0u8; 16];
    recv_iovec[0..8].copy_from_slice(&0x10B10u64.to_le_bytes());
    recv_iovec[8..16].copy_from_slice(&8u64.to_le_bytes());
    scratch.lock().write_bytes(0x10A40, &recv_iovec).unwrap();
    let mut recv_header = [0u8; 56];
    recv_header[16..24].copy_from_slice(&0x10A40u64.to_le_bytes());
    recv_header[24..32].copy_from_slice(&1u64.to_le_bytes());
    recv_header[32..40].copy_from_slice(&0x10A50u64.to_le_bytes());
    recv_header[40..48].copy_from_slice(&16u64.to_le_bytes());
    scratch.lock().write_bytes(0x10A00, &recv_header).unwrap();
    assert_eq!(
        dispatch(SYS_RECVMSG, rights_right as u64, 0x10A00, 0, 0, 0, 0),
        1
    );
    assert_eq!(read_i32(0x10A30) & 0x20, 0x20);
    assert_eq!(read_u32(0x10A28), 0);

    // MSG_CMSG_CLOEXEC marks the installed descriptor close-on-exec.
    assert_eq!(
        dispatch(SYS_SENDMSG, rights_left as u64, 0x10900, 0, 0, 0, 0),
        1
    );
    let mut recv_header = [0u8; 56];
    recv_header[16..24].copy_from_slice(&0x10A40u64.to_le_bytes());
    recv_header[24..32].copy_from_slice(&1u64.to_le_bytes());
    recv_header[32..40].copy_from_slice(&0x10A50u64.to_le_bytes());
    recv_header[40..48].copy_from_slice(&64u64.to_le_bytes());
    scratch.lock().write_bytes(0x10A00, &recv_header).unwrap();
    assert_eq!(
        dispatch(
            SYS_RECVMSG,
            rights_right as u64,
            0x10A00,
            0x4000_0000,
            0,
            0,
            0
        ),
        1
    );
    assert_eq!(read_i32(0x10A30) & 0x20, 0);
    let mut control = [0u8; 20];
    scratch.lock().read_bytes(0x10A50, &mut control).unwrap();
    let received_fd = i32::from_le_bytes(control[16..20].try_into().unwrap());
    assert!(received_fd >= 0);
    assert_eq!(
        dispatch(
            SYS_FCNTL,
            received_fd as u64,
            abi::F_GETFD as u64,
            0,
            0,
            0,
            0
        ),
        1
    );
    assert_eq!(dispatch(SYS_CLOSE, received_fd as u64, 0, 0, 0, 0, 0), 0);

    // SO_PASSCRED delivers SCM_CREDENTIALS.
    crate::verbose::log(format_args!("ipc selftest: credentials"));
    scratch
        .lock()
        .write_bytes(0x10810, &1i32.to_le_bytes())
        .unwrap();
    assert_eq!(
        dispatch(SYS_SETSOCKOPT, rights_right as u64, 1, 16, 0x10810, 4, 0),
        0
    );
    let mut credentials_cmsg = [0u8; 28];
    credentials_cmsg[0..8].copy_from_slice(&28u64.to_le_bytes());
    credentials_cmsg[8..12].copy_from_slice(&1i32.to_le_bytes());
    credentials_cmsg[12..16].copy_from_slice(&2i32.to_le_bytes());
    scratch
        .lock()
        .write_bytes(0x10950, &credentials_cmsg)
        .unwrap();
    let mut send_header = [0u8; 56];
    send_header[16..24].copy_from_slice(&0x10940u64.to_le_bytes());
    send_header[24..32].copy_from_slice(&1u64.to_le_bytes());
    send_header[32..40].copy_from_slice(&0x10950u64.to_le_bytes());
    send_header[40..48].copy_from_slice(&28u64.to_le_bytes());
    scratch.lock().write_bytes(0x10900, &send_header).unwrap();
    assert_eq!(
        dispatch(SYS_SENDMSG, rights_left as u64, 0x10900, 0, 0, 0, 0),
        1
    );
    let mut recv_header = [0u8; 56];
    recv_header[16..24].copy_from_slice(&0x10A40u64.to_le_bytes());
    recv_header[24..32].copy_from_slice(&1u64.to_le_bytes());
    recv_header[32..40].copy_from_slice(&0x10A50u64.to_le_bytes());
    recv_header[40..48].copy_from_slice(&64u64.to_le_bytes());
    scratch.lock().write_bytes(0x10A00, &recv_header).unwrap();
    assert_eq!(
        dispatch(SYS_RECVMSG, rights_right as u64, 0x10A00, 0, 0, 0, 0),
        1
    );
    let mut control = [0u8; 28];
    scratch.lock().read_bytes(0x10A50, &mut control).unwrap();
    assert_eq!(u64::from_le_bytes(control[0..8].try_into().unwrap()), 28);
    assert_eq!(i32::from_le_bytes(control[8..12].try_into().unwrap()), 1);
    assert_eq!(i32::from_le_bytes(control[12..16].try_into().unwrap()), 2);
    assert_eq!(
        u32::from_le_bytes(control[16..20].try_into().unwrap()),
        task.stats().tgid as u32
    );

    // fcntl(F_SETFL, O_NONBLOCK) changes socket blocking behavior.
    crate::verbose::log(format_args!("ipc selftest: nonblock"));
    assert_eq!(
        dispatch(
            SYS_FCNTL,
            rights_right as u64,
            abi::F_SETFL as u64,
            abi::O_NONBLOCK as u64,
            0,
            0,
            0
        ),
        0
    );
    assert_ne!(
        dispatch(
            SYS_FCNTL,
            rights_right as u64,
            abi::F_GETFL as u64,
            0,
            0,
            0,
            0
        ) & abi::O_NONBLOCK as i64,
        0
    );
    assert_eq!(
        dispatch(SYS_READ, rights_right as u64, 0x10B20, 8, 0, 0, 0),
        -abi::EAGAIN
    );

    for fd in [
        server,
        client,
        accepted,
        abstract_server,
        abstract_client,
        abstract_accepted,
        late_client,
        rights_left as i64,
        rights_right as i64,
        carried,
    ] {
        assert_eq!(dispatch(SYS_CLOSE, fd as u64, 0, 0, 0, 0, 0), 0);
    }

    // Verify getrandom matches hardware RNG support without hanging
    let gr = dispatch(SYS_GETRANDOM, 0x10200, 16, 0, 0, 0, 0);
    if crate::random::supported() {
        assert_eq!(gr, 16);
    } else {
        assert_eq!(gr, -abi::EAGAIN);
    }

    vfs::write_file("/tmp/invalid-exec", b"not ELF").unwrap();
    vfs::lookup_abs("/tmp/invalid-exec", true)
        .unwrap()
        .0
        .chmod(0o755)
        .unwrap();
    scratch
        .lock()
        .write_bytes(0x10000, b"/tmp/invalid-exec\0")
        .unwrap();
    let root = scratch.lock().root_physical();
    assert_eq!(dispatch(SYS_EXECVE, 0x10000, 0, 0, 0, 0, 0), -8);
    assert_eq!(task.address_space().unwrap().lock().root_physical(), root);
    vfs::unlink_abs("/tmp/invalid-exec", false).unwrap();
    task.lock().address_space = old.clone();
    if let Some(old) = old {
        old.lock().activate();
    } else {
        UserAddressSpace::deactivate();
    }
    drop(scratch);
    crate::verbose::log(format_args!(
        "user mmap/mprotect/brk/EFAULT/exec rollback self-test passed"
    ));
}

/// epoll, signalfd and timerfd semantics through the real syscall dispatcher.
#[cfg(feature = "boot-self-test")]
pub fn ipc_advanced() {
    use crate::{
        signal::{self, SIGUSR1},
        sync::SpinLock,
        syscall::{dispatch, numbers::*},
        vfs::{self, abi},
    };
    use alloc::sync::Arc;

    let task = crate::sched::current().unwrap();
    let old = task.address_space();
    let scratch = Arc::new(SpinLock::new(UserAddressSpace::new().unwrap()));
    scratch.lock().map_zeroed(0x10000, 0x4000, 3).unwrap();
    task.lock().set_address_space(scratch.clone());
    scratch.lock().activate();

    let epoll_wait_call = |epfd: i64, events: u64, maxevents: u32, timeout: i64| -> i64 {
        #[cfg(target_arch = "x86_64")]
        {
            dispatch(
                SYS_EPOLL_WAIT,
                epfd as u64,
                events,
                maxevents as u64,
                timeout as u64,
                0,
                0,
            )
        }
        #[cfg(not(target_arch = "x86_64"))]
        {
            dispatch(
                SYS_EPOLL_PWAIT,
                epfd as u64,
                events,
                maxevents as u64,
                timeout as u64,
                0,
                8,
            )
        }
    };
    let event_record = |events: u32, data: u64| -> [u8; 12] {
        let mut record = [0u8; 12];
        record[0..4].copy_from_slice(&events.to_le_bytes());
        record[4..12].copy_from_slice(&data.to_le_bytes());
        record
    };

    // Level-triggered epoll over an eventfd.
    let efd = dispatch(SYS_EVENTFD2, 0, abi::O_NONBLOCK as u64, 0, 0, 0, 0);
    assert!(efd >= 0);
    let epfd = dispatch(SYS_EPOLL_CREATE1, 0, 0, 0, 0, 0, 0);
    assert!(epfd >= 0, "epoll_create1 returned {}", epfd);
    scratch
        .lock()
        .write_bytes(0x10000, &event_record(1, 0x1234))
        .unwrap();
    assert_eq!(
        dispatch(SYS_EPOLL_CTL, epfd as u64, 1, efd as u64, 0x10000, 0, 0),
        0
    );
    assert_eq!(
        dispatch(SYS_EPOLL_CTL, epfd as u64, 1, efd as u64, 0x10000, 0, 0),
        -abi::EEXIST
    );
    assert_eq!(
        dispatch(SYS_EPOLL_CTL, epfd as u64, 1, epfd as u64, 0x10000, 0, 0),
        -abi::EINVAL
    );
    assert_eq!(epoll_wait_call(epfd, 0x10100, 8, 0), 0);
    scratch
        .lock()
        .write_bytes(0x10200, &5u64.to_le_bytes())
        .unwrap();
    assert_eq!(dispatch(SYS_WRITE, efd as u64, 0x10200, 8, 0, 0, 0), 8);
    assert_eq!(epoll_wait_call(epfd, 0x10100, 8, 1000), 1);
    let mut record = [0u8; 12];
    scratch.lock().read_bytes(0x10100, &mut record).unwrap();
    assert_ne!(u32::from_le_bytes(record[0..4].try_into().unwrap()) & 1, 0);
    assert_eq!(
        u64::from_le_bytes(record[4..12].try_into().unwrap()),
        0x1234
    );
    assert_eq!(dispatch(SYS_READ, efd as u64, 0x10210, 8, 0, 0, 0), 8);

    // Edge-triggered reports only after readiness was drained and set again.
    let ep2 = dispatch(SYS_EPOLL_CREATE1, 0, 0, 0, 0, 0, 0);
    assert!(ep2 >= 0);
    scratch
        .lock()
        .write_bytes(0x10020, &event_record(1 | 0x8000_0000, 0x55))
        .unwrap();
    assert_eq!(
        dispatch(SYS_EPOLL_CTL, ep2 as u64, 1, efd as u64, 0x10020, 0, 0),
        0
    );
    scratch
        .lock()
        .write_bytes(0x10200, &1u64.to_le_bytes())
        .unwrap();
    assert_eq!(dispatch(SYS_WRITE, efd as u64, 0x10200, 8, 0, 0, 0), 8);
    assert_eq!(epoll_wait_call(ep2, 0x10100, 8, 1000), 1);
    assert_eq!(dispatch(SYS_READ, efd as u64, 0x10210, 8, 0, 0, 0), 8);
    assert_eq!(epoll_wait_call(ep2, 0x10100, 8, 100), 0);
    scratch
        .lock()
        .write_bytes(0x10200, &1u64.to_le_bytes())
        .unwrap();
    assert_eq!(dispatch(SYS_WRITE, efd as u64, 0x10200, 8, 0, 0, 0), 8);
    assert_eq!(epoll_wait_call(ep2, 0x10100, 8, 1000), 1);
    assert_eq!(dispatch(SYS_READ, efd as u64, 0x10210, 8, 0, 0, 0), 8);

    // ONESHOT disables the interest until MOD re-arms it.
    let ep3 = dispatch(SYS_EPOLL_CREATE1, 0, 0, 0, 0, 0, 0);
    assert!(ep3 >= 0);
    scratch
        .lock()
        .write_bytes(0x10040, &event_record(1 | 0x4000_0000, 0x66))
        .unwrap();
    assert_eq!(
        dispatch(SYS_EPOLL_CTL, ep3 as u64, 1, efd as u64, 0x10040, 0, 0),
        0
    );
    scratch
        .lock()
        .write_bytes(0x10200, &1u64.to_le_bytes())
        .unwrap();
    assert_eq!(dispatch(SYS_WRITE, efd as u64, 0x10200, 8, 0, 0, 0), 8);
    assert_eq!(epoll_wait_call(ep3, 0x10100, 8, 1000), 1);
    assert_eq!(dispatch(SYS_READ, efd as u64, 0x10210, 8, 0, 0, 0), 8);
    scratch
        .lock()
        .write_bytes(0x10200, &1u64.to_le_bytes())
        .unwrap();
    assert_eq!(dispatch(SYS_WRITE, efd as u64, 0x10200, 8, 0, 0, 0), 8);
    assert_eq!(epoll_wait_call(ep3, 0x10100, 8, 100), 0);
    assert_eq!(
        dispatch(SYS_EPOLL_CTL, ep3 as u64, 3, efd as u64, 0x10040, 0, 0),
        0
    );
    assert_eq!(epoll_wait_call(ep3, 0x10100, 8, 1000), 1);
    assert_eq!(dispatch(SYS_READ, efd as u64, 0x10210, 8, 0, 0, 0), 8);
    assert_eq!(
        dispatch(SYS_EPOLL_CTL, epfd as u64, 2, efd as u64, 0, 0, 0),
        0
    );
    assert_eq!(
        dispatch(SYS_EPOLL_CTL, epfd as u64, 2, efd as u64, 0, 0, 0),
        -abi::ENOENT
    );

    // signalfd consumes a pending masked signal as a 128-byte record.
    scratch
        .lock()
        .write_bytes(0x10300, &(1u64 << (SIGUSR1 - 1)).to_le_bytes())
        .unwrap();
    let sfd = dispatch(
        SYS_SIGNALFD4,
        (-1i64) as u64,
        0x10300,
        8,
        abi::O_NONBLOCK as u64,
        0,
        0,
    );
    assert!(sfd >= 0);
    signal::send_to_thread(&task, signal::Signal::kernel(SIGUSR1));
    #[cfg(target_arch = "x86_64")]
    {
        let mut pollfd = [0u8; 8];
        pollfd[0..4].copy_from_slice(&(sfd as i32).to_le_bytes());
        pollfd[4..6].copy_from_slice(&1u16.to_le_bytes());
        scratch.lock().write_bytes(0x10380, &pollfd).unwrap();
        assert_eq!(dispatch(SYS_POLL, 0x10380, 1, 1000, 0, 0, 0), 1);
        scratch.lock().read_bytes(0x10380, &mut pollfd).unwrap();
        assert_ne!(u16::from_le_bytes(pollfd[6..8].try_into().unwrap()) & 1, 0);
    }
    assert_eq!(dispatch(SYS_READ, sfd as u64, 0x10400, 128, 0, 0, 0), 128);
    let mut info = [0u8; 128];
    scratch.lock().read_bytes(0x10400, &mut info).unwrap();
    assert_eq!(u32::from_le_bytes(info[0..4].try_into().unwrap()), SIGUSR1);
    assert_eq!(u32::from_le_bytes(info[8..12].try_into().unwrap()), 0x80);
    assert_eq!(
        dispatch(SYS_READ, sfd as u64, 0x10400, 128, 0, 0, 0),
        -abi::EAGAIN
    );
    assert_eq!(dispatch(SYS_CLOSE, sfd as u64, 0, 0, 0, 0, 0), 0);

    // timerfd: one-shot expiry, periodic ticks and gettime.
    let tfd = dispatch(SYS_TIMERFD_CREATE, 1, abi::O_NONBLOCK as u64, 0, 0, 0, 0);
    assert!(tfd >= 0);
    let mut spec = [0u64; 4];
    spec[3] = 20_000_000; // value = 20 ms, interval first in itimerspec
    scratch
        .lock()
        .write_bytes(0x10500, unsafe {
            core::slice::from_raw_parts(spec.as_ptr() as *const u8, 32)
        })
        .unwrap();
    assert_eq!(
        dispatch(SYS_TIMERFD_SETTIME, tfd as u64, 0, 0x10500, 0, 0, 0),
        0
    );
    assert_eq!(
        dispatch(SYS_READ, tfd as u64, 0x10580, 8, 0, 0, 0),
        -abi::EAGAIN
    );
    crate::sched::sleep_micros(25_000);
    assert_eq!(dispatch(SYS_READ, tfd as u64, 0x10580, 8, 0, 0, 0), 8);
    let mut ticks = [0u8; 8];
    scratch.lock().read_bytes(0x10580, &mut ticks).unwrap();
    assert!(u64::from_le_bytes(ticks) >= 1);
    let mut periodic = [0u64; 4];
    periodic[1] = 10_000_000; // interval = 10 ms
    periodic[3] = 10_000_000;
    scratch
        .lock()
        .write_bytes(0x10520, unsafe {
            core::slice::from_raw_parts(periodic.as_ptr() as *const u8, 32)
        })
        .unwrap();
    assert_eq!(
        dispatch(SYS_TIMERFD_SETTIME, tfd as u64, 0, 0x10520, 0, 0, 0),
        0
    );
    crate::sched::sleep_micros(35_000);
    assert_eq!(dispatch(SYS_READ, tfd as u64, 0x10580, 8, 0, 0, 0), 8);
    scratch.lock().read_bytes(0x10580, &mut ticks).unwrap();
    assert!(u64::from_le_bytes(ticks) >= 3);
    assert_eq!(
        dispatch(SYS_TIMERFD_GETTIME, tfd as u64, 0x105A0, 0, 0, 0, 0),
        0
    );
    let mut current = [0u8; 32];
    scratch.lock().read_bytes(0x105A0, &mut current).unwrap();
    assert_eq!(
        u64::from_le_bytes(current[8..16].try_into().unwrap()),
        10_000_000
    );

    // Nonblocking pipe: capacity, PIPE_BUF atomicity failure, poll, EOF.
    assert_eq!(
        dispatch(SYS_PIPE2, 0x10600, abi::O_NONBLOCK as u64, 0, 0, 0, 0),
        0
    );
    let mut pipefds = [0i32; 2];
    scratch
        .lock()
        .read_bytes(0x10600, unsafe {
            core::slice::from_raw_parts_mut(pipefds.as_mut_ptr() as *mut u8, 8)
        })
        .unwrap();
    scratch
        .lock()
        .write_bytes(0x10700, &[0x5au8; 4096])
        .unwrap();
    for _ in 0..16 {
        assert_eq!(
            dispatch(SYS_WRITE, pipefds[1] as u64, 0x10700, 4096, 0, 0, 0),
            4096
        );
    }
    assert_eq!(
        dispatch(SYS_WRITE, pipefds[1] as u64, 0x10700, 1, 0, 0, 0),
        -abi::EAGAIN,
        "PIPE_BUF write must not split when the pipe is full"
    );
    #[cfg(target_arch = "x86_64")]
    {
        let mut pollfd = [0u8; 8];
        pollfd[0..4].copy_from_slice(&pipefds[1].to_le_bytes());
        pollfd[4..6].copy_from_slice(&4u16.to_le_bytes());
        scratch.lock().write_bytes(0x10680, &pollfd).unwrap();
        assert_eq!(dispatch(SYS_POLL, 0x10680, 1, 0, 0, 0, 0), 0);
        assert_eq!(
            dispatch(SYS_READ, pipefds[0] as u64, 0x10800, 4096, 0, 0, 0),
            4096
        );
        scratch.lock().write_bytes(0x10680, &pollfd).unwrap();
        assert_eq!(dispatch(SYS_POLL, 0x10680, 1, 0, 0, 0, 0), 1);
    }
    assert_eq!(dispatch(SYS_CLOSE, pipefds[1] as u64, 0, 0, 0, 0, 0), 0);
    #[cfg(target_arch = "x86_64")]
    {
        let mut pollfd = [0u8; 8];
        pollfd[0..4].copy_from_slice(&pipefds[0].to_le_bytes());
        pollfd[4..6].copy_from_slice(&1u16.to_le_bytes());
        scratch.lock().write_bytes(0x10680, &pollfd).unwrap();
        assert_eq!(dispatch(SYS_POLL, 0x10680, 1, 0, 0, 0, 0), 1);
        scratch.lock().read_bytes(0x10680, &mut pollfd).unwrap();
        assert_ne!(
            u16::from_le_bytes(pollfd[6..8].try_into().unwrap()) & 0x10,
            0
        );
    }
    let mut total = 0;
    for _ in 0..32 {
        let count = dispatch(SYS_READ, pipefds[0] as u64, 0x10800, 4096, 0, 0, 0);
        if count == 0 {
            break;
        }
        total += count;
    }
    assert_eq!(total, 65536 - 4096);
    assert_eq!(
        dispatch(SYS_READ, pipefds[0] as u64, 0x10800, 4096, 0, 0, 0),
        0
    );
    assert_eq!(dispatch(SYS_CLOSE, pipefds[0] as u64, 0, 0, 0, 0, 0), 0);

    // A blocking read wakes when another task writes to the same open file.
    assert_eq!(dispatch(SYS_PIPE2, 0x10610, 0, 0, 0, 0, 0), 0);
    let mut blocking = [0i32; 2];
    scratch
        .lock()
        .read_bytes(0x10610, unsafe {
            core::slice::from_raw_parts_mut(blocking.as_mut_ptr() as *mut u8, 8)
        })
        .unwrap();
    let writer = vfs::current_fd_table()
        .lock()
        .get(blocking[1] as usize)
        .unwrap()
        .file;
    crate::sched::spawn(
        move || {
            crate::sched::sleep_micros(10_000);
            let _ = writer.write(b"z");
        },
        0,
    )
    .unwrap();
    assert_eq!(
        dispatch(SYS_READ, blocking[0] as u64, 0x10620, 1, 0, 0, 0),
        1
    );
    let mut byte = [0u8; 1];
    scratch.lock().read_bytes(0x10620, &mut byte).unwrap();
    assert_eq!(byte[0], b'z');
    assert_eq!(dispatch(SYS_CLOSE, blocking[0] as u64, 0, 0, 0, 0, 0), 0);
    assert_eq!(dispatch(SYS_CLOSE, blocking[1] as u64, 0, 0, 0, 0, 0), 0);

    // memfd: read/write, ftruncate, MAP_SHARED coherence and seals.
    scratch
        .lock()
        .write_bytes(0x10900, b"/memfd-test\0")
        .unwrap();
    let mfd = dispatch(SYS_MEMFD_CREATE, 0x10900, 2, 0, 0, 0, 0);
    assert!(mfd >= 0, "memfd_create={}", mfd);
    scratch.lock().write_bytes(0x10920, b"hello").unwrap();
    assert_eq!(dispatch(SYS_WRITE, mfd as u64, 0x10920, 5, 0, 0, 0), 5);
    assert_eq!(dispatch(SYS_LSEEK, mfd as u64, 0, 0, 0, 0, 0), 0);
    assert_eq!(dispatch(SYS_READ, mfd as u64, 0x10930, 5, 0, 0, 0), 5);
    let mut mem = [0u8; 16];
    scratch.lock().read_bytes(0x10930, &mut mem[..5]).unwrap();
    assert_eq!(&mem[..5], b"hello");
    assert_eq!(dispatch(SYS_FTRUNCATE, mfd as u64, 8192, 0, 0, 0, 0), 0);
    let mapped = dispatch(SYS_MMAP, 0, 8192, 3, 1, mfd as u64, 0);
    assert!(mapped > 0, "memfd mmap={}", mapped);
    crate::user::uaccess::prefault(mapped as u64, 8192, true).unwrap();
    scratch
        .lock()
        .write_bytes(mapped as u64, b"SHARED")
        .unwrap();
    assert_eq!(dispatch(SYS_LSEEK, mfd as u64, 0, 0, 0, 0, 0), 0);
    assert_eq!(dispatch(SYS_READ, mfd as u64, 0x10940, 6, 0, 0, 0), 6);
    scratch.lock().read_bytes(0x10940, &mut mem[..6]).unwrap();
    assert_eq!(&mem[..6], b"SHARED");
    scratch
        .lock()
        .read_bytes(mapped as u64, &mut mem[..6])
        .unwrap();
    assert_eq!(&mem[..6], b"SHARED");
    // SHRINK|GROW seals block truncate in both directions.
    let seal_result = dispatch(SYS_FCNTL, mfd as u64, 1033, 6, 0, 0, 0);
    assert!(seal_result == 0, "F_ADD_SEALS returned {}", seal_result);
    let grow_result = dispatch(SYS_FTRUNCATE, mfd as u64, 16384, 0, 0, 0, 0);
    assert!(
        grow_result == -abi::EPERM,
        "grow truncate returned {}",
        grow_result
    );
    assert_eq!(
        dispatch(SYS_FTRUNCATE, mfd as u64, 4096, 0, 0, 0, 0),
        -abi::EPERM
    );
    // A writable shared mapping is still live, so F_SEAL_WRITE is EBUSY.
    let busy_result = dispatch(SYS_FCNTL, mfd as u64, 1033, 8, 0, 0, 0);
    assert!(
        busy_result == -abi::EBUSY,
        "F_ADD_SEALS WRITE returned {}",
        busy_result
    );
    assert_eq!(dispatch(SYS_FCNTL, mfd as u64, 1034, 0, 0, 0, 0), 6);
    assert_eq!(dispatch(SYS_MUNMAP, mapped as u64, 8192, 0, 0, 0, 0), 0);
    assert_eq!(dispatch(SYS_CLOSE, mfd as u64, 0, 0, 0, 0, 0), 0);

    // Without MFD_ALLOW_SEALING the file starts F_SEAL_SEAL.
    scratch
        .lock()
        .write_bytes(0x10900, b"/memfd-plain\0")
        .unwrap();
    let plain = dispatch(SYS_MEMFD_CREATE, 0x10900, 0, 0, 0, 0, 0);
    assert!(plain >= 0);
    assert_eq!(
        dispatch(SYS_FCNTL, plain as u64, 1033, 1, 0, 0, 0),
        -abi::EPERM
    );
    assert_eq!(dispatch(SYS_FCNTL, plain as u64, 1034, 0, 0, 0, 0), 1);
    assert_eq!(dispatch(SYS_CLOSE, plain as u64, 0, 0, 0, 0, 0), 0);

    // F_SEAL_FUTURE_WRITE blocks later write() and writable shared mmap.
    scratch
        .lock()
        .write_bytes(0x10900, b"/memfd-future\0")
        .unwrap();
    let future = dispatch(SYS_MEMFD_CREATE, 0x10900, 2, 0, 0, 0, 0);
    assert!(future >= 0);
    assert_eq!(dispatch(SYS_FTRUNCATE, future as u64, 4096, 0, 0, 0, 0), 0);
    assert_eq!(dispatch(SYS_FCNTL, future as u64, 1033, 16, 0, 0, 0), 0);
    assert_eq!(
        dispatch(SYS_WRITE, future as u64, 0x10920, 1, 0, 0, 0),
        -abi::EPERM
    );
    assert_eq!(
        dispatch(SYS_MMAP, 0, 4096, 3, 1, future as u64, 0),
        -abi::EPERM
    );
    assert_eq!(dispatch(SYS_CLOSE, future as u64, 0, 0, 0, 0, 0), 0);

    // FIFO: mknod, nonblocking ENXIO without a reader, data and EPIPE.
    scratch
        .lock()
        .write_bytes(0x10900, b"/tmp/fifo-test\0")
        .unwrap();
    assert_eq!(
        dispatch(
            SYS_MKNODAT,
            abi::AT_FDCWD as i64 as u64,
            0x10900,
            (0o010000 | 0o666) as u64,
            0,
            0,
            0
        ),
        0
    );
    assert_eq!(
        vfs::lookup_abs("/tmp/fifo-test", true)
            .unwrap()
            .0
            .metadata()
            .unwrap()
            .kind,
        vfs::InodeKind::Fifo
    );
    assert_eq!(
        dispatch(
            SYS_OPENAT,
            abi::AT_FDCWD as i64 as u64,
            0x10900,
            (abi::O_WRONLY | abi::O_NONBLOCK) as u64,
            0,
            0,
            0
        ),
        -6,
        "O_WRONLY|O_NONBLOCK without a reader must return ENXIO"
    );
    let fifo_reader = dispatch(
        SYS_OPENAT,
        abi::AT_FDCWD as i64 as u64,
        0x10900,
        (abi::O_RDONLY | abi::O_NONBLOCK) as u64,
        0,
        0,
        0,
    );
    assert!(fifo_reader >= 0);
    let fifo_writer = dispatch(
        SYS_OPENAT,
        abi::AT_FDCWD as i64 as u64,
        0x10900,
        (abi::O_WRONLY | abi::O_NONBLOCK) as u64,
        0,
        0,
        0,
    );
    assert!(fifo_writer >= 0);
    scratch.lock().write_bytes(0x10920, b"abc").unwrap();
    assert_eq!(
        dispatch(SYS_WRITE, fifo_writer as u64, 0x10920, 3, 0, 0, 0),
        3
    );
    assert_eq!(
        dispatch(SYS_READ, fifo_reader as u64, 0x10930, 3, 0, 0, 0),
        3
    );
    scratch.lock().read_bytes(0x10930, &mut mem[..3]).unwrap();
    assert_eq!(&mem[..3], b"abc");
    assert_eq!(dispatch(SYS_CLOSE, fifo_reader as u64, 0, 0, 0, 0, 0), 0);
    assert_eq!(
        dispatch(SYS_WRITE, fifo_writer as u64, 0x10920, 1, 0, 0, 0),
        -abi::EPIPE
    );
    assert_eq!(dispatch(SYS_CLOSE, fifo_writer as u64, 0, 0, 0, 0, 0), 0);
    assert_eq!(
        dispatch(
            SYS_UNLINKAT,
            abi::AT_FDCWD as i64 as u64,
            0x10900,
            0,
            0,
            0,
            0
        ),
        0
    );
    assert_eq!(
        dispatch(
            SYS_OPENAT,
            abi::AT_FDCWD as i64 as u64,
            0x10900,
            abi::O_RDONLY as u64,
            0,
            0,
            0
        ),
        -abi::ENOENT
    );

    // Unconnected stream sockets report ENOTCONN for send/recv.
    let unconnected = dispatch(SYS_SOCKET, 1, 1, 0, 0, 0, 0);
    assert!(unconnected >= 0);
    scratch.lock().write_bytes(0x10920, b"x").unwrap();
    assert_eq!(
        dispatch(SYS_WRITE, unconnected as u64, 0x10920, 1, 0, 0, 0),
        -abi::ENOTCONN
    );
    assert_eq!(
        dispatch(SYS_READ, unconnected as u64, 0x10930, 1, 0, 0, 0),
        -abi::ENOTCONN
    );
    assert_eq!(dispatch(SYS_CLOSE, unconnected as u64, 0, 0, 0, 0, 0), 0);

    for fd in [epfd, ep2, ep3, efd, tfd] {
        assert_eq!(dispatch(SYS_CLOSE, fd as u64, 0, 0, 0, 0, 0), 0);
    }

    task.lock().address_space = old.clone();
    if let Some(old) = old {
        old.lock().activate();
    } else {
        UserAddressSpace::deactivate();
    }
    drop(scratch);
    crate::verbose::log(format_args!(
        "epoll/signalfd/timerfd syscall self-test passed"
    ));
}
