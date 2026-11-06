/*
 *       src/vfs/pseudo_test.rs
 *       Device publication, namespace mutation and procfs lifetime checks
 *       2026/10/24 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 */
use super::*;
use alloc::{format, sync::Arc};
use core::sync::atomic::{AtomicU64, Ordering};
struct TestDevice;
impl Inode for TestDevice {
    fn metadata(&self) -> Result<Stat, VfsError> {
        let mut s = Stat::simple(1, InodeKind::CharDevice, 0o600, 0);
        s.rdev = 240 << 8;
        Ok(s)
    }
    fn read_at(&self, _: u64, bytes: &mut [u8]) -> Result<usize, VfsError> {
        bytes.fill(0xa5);
        Ok(bytes.len())
    }
}
pub fn devices() {
    let counter = Arc::new(AtomicU64::new(0));
    let c = counter.clone();
    let device = crate::device::register(
        "pseudo_test",
        "test",
        Some("test/sub0"),
        Some(Arc::new(TestDevice)),
        alloc::vec![(
            "counter",
            tree::Attribute::new(0o444, move || Ok(format!(
                "snapshot-{}\n",
                c.fetch_add(1, Ordering::Relaxed)
            )
            .into_bytes()))
        )],
    )
    .unwrap();
    let file = open_abs("/dev/test/sub0", abi::O_RDONLY, 0).unwrap();
    let original = file.inode.metadata().unwrap();
    file.inode.chmod(0o640).unwrap();
    file.inode.chown(123, 456).unwrap();
    let again = lookup_abs("/dev/test/sub0", true)
        .unwrap()
        .0
        .metadata()
        .unwrap();
    assert_eq!(again.ino, original.ino);
    assert_eq!((again.mode, again.uid, again.gid), (0o640, 123, 456));
    assert_eq!(
        readlink_abs("/sys/class/test/pseudo_test").unwrap(),
        "../../devices/pseudo_test"
    );
    assert!(lookup_abs("/sys/dev/char/240:0/dev", true).is_ok());
    let attribute = open_abs("/sys/devices/pseudo_test/counter", abi::O_RDONLY, 0).unwrap();
    let mut buffer = [0; 64];
    let a = attribute.read(&mut buffer[..3]).unwrap();
    let b = attribute.read(&mut buffer[a..]).unwrap();
    assert_eq!(&buffer[..a + b], b"snapshot-0\n");
    assert_eq!(counter.load(Ordering::Relaxed), 1);
    assert_eq!(attribute.seek(0, abi::SEEK_SET).unwrap(), 0);
    let c_len = attribute.read(&mut buffer).unwrap();
    assert_eq!(&buffer[..c_len], b"snapshot-0\n");
    assert_eq!(counter.load(Ordering::Relaxed), 1);
    assert!(write_file("/sys/devices/pseudo_test/new", b"invalid").is_err());

    // Verify uevent sequence numbers and writable uevent trigger
    let seq_before: u64 = core::str::from_utf8(&read_file("/sys/kernel/uevent_seqnum").unwrap())
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(write_file("/sys/devices/pseudo_test/uevent", b"invalid\n").is_err());
    write_file("/sys/devices/pseudo_test/uevent", b"change\n").unwrap();
    let seq_after: u64 = core::str::from_utf8(&read_file("/sys/kernel/uevent_seqnum").unwrap())
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(seq_after > seq_before);
    let events = crate::device::events_after(seq_before);
    assert!(
        events
            .iter()
            .any(|e| e.action == "change" && e.path == "/devices/pseudo_test")
    );

    // Verify conflict rollback leaves no stray directories in /dev
    assert!(
        crate::device::register(
            "pseudo_test",
            "test",
            Some("rollback_dir/node0"),
            Some(Arc::new(TestDevice)),
            alloc::vec![]
        )
        .is_err()
    );
    assert!(lookup_abs("/dev/rollback_dir", false).is_err());

    // Verify chmod 0o000 blocks subsequent open_abs while existing open file continues
    file.inode.chmod(0o000).unwrap();
    assert_eq!(
        open_abs("/dev/test/sub0", abi::O_RDONLY, 0).err(),
        Some(VfsError::PermissionDenied)
    );
    file.inode.chmod(0o640).unwrap();

    let dangling_attr = lookup_abs("/sys/devices/pseudo_test/counter", true)
        .unwrap()
        .0;
    unlink_abs("/dev/test/sub0", false).unwrap();
    write_file("/dev/test/sub0", b"user replacement").unwrap();
    crate::device::unregister(device.id).unwrap();
    assert_eq!(file.read(&mut buffer).unwrap_err(), VfsError::NoDevice);
    assert_eq!(
        dangling_attr.read_at(0, &mut buffer).unwrap_err(),
        VfsError::NoDevice
    );
    assert_eq!(read_file("/dev/test/sub0").unwrap(), b"user replacement");
    assert!(lookup_abs("/sys/devices/pseudo_test", true).is_err());
    assert!(lookup_abs("/sys/dev/char/240:0", true).is_err());
    unlink_abs("/dev/test/sub0", false).unwrap();
    unlink_abs("/dev/test", true).unwrap();
    let a = devtmpfs::DevTmpFs.root_inode();
    let b = devtmpfs::DevTmpFs.root_inode();
    assert_eq!(a.metadata().unwrap().ino, b.metadata().unwrap().ino);

    // /dev/kmsg Linux standard interface verification
    let kmsg =
        open_abs("/dev/kmsg", abi::O_RDWR | abi::O_NONBLOCK, 0).expect("/dev/kmsg should exist");
    assert_eq!(kmsg.inode.metadata().unwrap().rdev, (1 << 8) | 11);

    // Seek to the end of buffer so we read newly written messages
    kmsg.seek(0, abi::SEEK_END).unwrap();

    let test_msg = b"<6>kmsg self-test message\n";
    let w = kmsg.write(test_msg).unwrap();
    assert_eq!(w, test_msg.len());
    let mut kbuf = [0u8; 512];
    let n = kmsg.read(&mut kbuf).unwrap();
    assert!(n > 0);
    let kmsg_str = core::str::from_utf8(&kbuf[..n]).unwrap();
    assert!(kmsg_str.contains("kmsg self-test message"));

    crate::verbose::log(format_args!(
        "devtmpfs/sysfs registry/snapshot/removal and /dev/kmsg self-test passed"
    ));
}
pub fn processes() {
    let tid = crate::sched::current().unwrap().stats().tid;
    assert_eq!(readlink_abs("/proc/self").unwrap(), format!("{}", tid));
    assert_eq!(
        readlink_abs("/proc/thread-self").unwrap(),
        format!("{}/task/{}", tid, tid)
    );
    assert!(lookup_abs("/proc/18446744073709551615", true).is_err());
    let status = read_file(&format!("/proc/{}/task/{}/status", tid, tid)).unwrap();
    assert!(status.windows(4).any(|p| p == b"Pid:"));
    let info = read_file("/proc/self/fdinfo/1").unwrap();
    assert!(info.starts_with(b"pos:"));
    assert!(readlink_abs("/proc/self/fd/1").is_ok());

    // Verify exited task disappears from /proc and retained proc inode returns NotFound
    let child_tid = crate::sched::spawn(|| {}, 0).unwrap();
    let proc_dir = lookup_abs(&format!("/proc/{}", child_tid), true).ok();
    for _ in 0..200 {
        if crate::sched::find_task(child_tid).is_none() {
            break;
        }
        crate::sched::sleep_micros(500);
    }
    assert!(lookup_abs(&format!("/proc/{}", child_tid), true).is_err());
    if let Some((inode, _)) = proc_dir {
        assert_eq!(inode.readdir().err(), Some(VfsError::NotFound));
    }
    crate::verbose::log(format_args!(
        "procfs identity/fd/task/symlink self-test passed"
    ));
}
