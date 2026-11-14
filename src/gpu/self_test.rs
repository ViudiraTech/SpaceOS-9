/*
 *       src/gpu/self_test.rs
 *       Real queue, Linux ioctl, GEM lifetime and VirGL readback checks
 *       2026/10/24 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 */
use super::*;
use crate::sync::Mutex;
const ARG: u64 = 0x10000;
fn call(
    file: &Arc<dyn Inode>,
    nr: u8,
    bytes: &mut [u8],
    space: &Arc<Mutex<UserAddressSpace>>,
) -> Result<(), VfsError> {
    space.lock().write_bytes(ARG, bytes)?;
    let (size, dir) = abi::descriptor(nr).unwrap();
    let cmd = ((dir as u64) << 30) | ((size as u64) << 16) | ((b'd' as u64) << 8) | nr as u64;
    file.ioctl(cmd, ARG)?;
    space.lock().read_bytes(ARG, bytes)?;
    Ok(())
}
fn empty(size: usize) -> Vec<u8> {
    vec![0; size]
}
pub fn run() -> &'static str {
    let Some(gpu) = device() else {
        return "none";
    };
    assert!(!gpu.transport.failed.load(Ordering::Acquire));
    let resources_before = gpu.live_resources();
    let task = crate::sched::current().unwrap();
    let old = task.address_space();
    let scratch = Arc::new(Mutex::new(UserAddressSpace::new().unwrap()));
    scratch.lock().map_zeroed(ARG, 65536, 3).unwrap();
    task.lock().set_address_space(scratch.clone());
    let first = vfs::open_abs("/dev/dri/card0", vfs::abi::O_RDWR, 0)
        .unwrap()
        .inode
        .clone();
    let second = vfs::open_abs("/dev/dri/renderD128", vfs::abi::O_RDWR, 0)
        .unwrap()
        .inode
        .clone();
    let mut version = empty(64);
    put64(&mut version, 16, 64);
    put64(&mut version, 24, 0x11000);
    call(&first, 0, &mut version, &scratch).unwrap();
    let mut name = [0; 10];
    scratch.lock().read_bytes(0x11000, &mut name[..9]).unwrap();
    assert_eq!(&name[..9], b"virtio_gp");
    let mut dumb = empty(32);
    put32(&mut dumb, 0, 64);
    put32(&mut dumb, 4, 64);
    put32(&mut dumb, 8, 32);
    call(&first, 0xb2, &mut dumb, &scratch).unwrap();
    let handle = u32_at(&dumb, 16).unwrap();
    assert_eq!(u64_at(&dumb, 24).unwrap(), 16384);
    let mut info = empty(16);
    put32(&mut info, 0, handle);
    assert!(call(&second, 0x45, &mut info, &scratch).is_err());
    call(&first, 0x45, &mut info, &scratch).unwrap();
    let mut map = empty(16);
    put32(&mut map, 8, handle);
    call(&first, 0x41, &mut map, &scratch).unwrap();
    let offset = u64_at(&map, 0).unwrap();
    first
        .mmap(&mut scratch.lock(), 0x40000, 4096, offset, 3)
        .unwrap();
    scratch
        .lock()
        .write_bytes(0x40000, b"GEM lifetime")
        .unwrap();
    let mut prime = empty(12);
    put32(&mut prime, 0, handle);
    put32(&mut prime, 4, vfs::abi::O_RDWR | vfs::abi::O_CLOEXEC);
    call(&first, 0x2d, &mut prime, &scratch).unwrap();
    let fd = u32_at(&prime, 8).unwrap();
    call(&second, 0x2e, &mut prime, &scratch).unwrap();
    let imported = u32_at(&prime, 0).unwrap();
    let mut close = empty(8);
    put32(&mut close, 0, handle);
    call(&first, 9, &mut close, &scratch).unwrap();
    let mut bytes = [0; 12];
    scratch.lock().read_bytes(0x40000, &mut bytes).unwrap();
    assert_eq!(&bytes, b"GEM lifetime");
    put32(&mut close, 0, imported);
    call(&second, 9, &mut close, &scratch).unwrap();
    vfs::current_fd_table().lock().close(fd as usize).unwrap();
    scratch.lock().unmap(0x40000, 4096).unwrap();
    let mut sync = empty(8);
    put32(&mut sync, 4, 1);
    call(&first, 0xbf, &mut sync, &scratch).unwrap();
    let sync_handle = u32_at(&sync, 0).unwrap();
    scratch
        .lock()
        .write_bytes(0x12000, &sync_handle.to_le_bytes())
        .unwrap();
    let mut wait = empty(40);
    put64(&mut wait, 0, 0x12000);
    put64(&mut wait, 8, crate::time::uptime_micros() * 1000);
    put32(&mut wait, 16, 1);
    call(&first, 0xc3, &mut wait, &scratch).unwrap();
    scratch
        .lock()
        .write_bytes(0x12010, &7u64.to_le_bytes())
        .unwrap();
    let mut timeline = empty(24);
    put64(&mut timeline, 0, 0x12000);
    put64(&mut timeline, 8, 0x12010);
    put32(&mut timeline, 16, 1);
    call(&first, 0xcd, &mut timeline, &scratch).unwrap();
    scratch
        .lock()
        .write_bytes(0x12010, &0u64.to_le_bytes())
        .unwrap();
    call(&first, 0xcb, &mut timeline, &scratch).unwrap();
    let mut point = [0; 8];
    scratch.lock().read_bytes(0x12010, &mut point).unwrap();
    assert_eq!(u64::from_le_bytes(point), 7);
    modeset(&gpu, &first, &scratch);
    let mut verified_virgl = false;
    let mut verified_venus = false;
    if gpu.transport.features & 1 != 0 {
        virgl(&gpu, &second, &scratch);
        verified_virgl = true;
    }
    if gpu.capsets.lock().iter().any(|c| c.id == 4) {
        venus(&gpu, &scratch);
        verified_venus = true;
    }
    let deadline = crate::time::uptime_micros() + 100000;
    while gpu.transport.irq_count.load(Ordering::Acquire) == 0
        && crate::time::uptime_micros() < deadline
    {
        crate::sched::sleep_micros(1000);
    }
    assert!(
        gpu.transport.irq_count.load(Ordering::Acquire) > 0,
        "VirtIO GPU IRQ never delivered"
    );
    drop(first);
    drop(second);
    assert!(
        gpu.wait_idle(resources_before, 3_000_000),
        "GPU did not quiesce after session close"
    );
    assert_eq!(
        gpu.live_resources(),
        resources_before,
        "GPU resources leaked after session close"
    );
    assert!(gpu.retired.lock().is_empty(), "GPU resources not retired");
    assert!(
        gpu.host_ranges.lock().is_empty(),
        "GPU host-visible BAR ranges leaked"
    );
    assert!(
        gpu.sessions.lock().iter().all(|w| w.strong_count() == 0),
        "DRM session leaked on close"
    );
    assert_eq!(
        gpu.master.load(Ordering::Acquire),
        0,
        "DRM master not released on close"
    );

    // Transient device removal & resource reclamation test
    {
        let transient_node = Arc::new(DeviceNode {
            gpu: gpu.clone(),
            render: true,
        });
        let reg = crate::device::register(
            "card_temp",
            "drm",
            Some("dri/card_temp"),
            Some(transient_node),
            alloc::vec![],
        )
        .unwrap();
        let temp_file = vfs::open_abs("/dev/dri/card_temp", vfs::abi::O_RDWR, 0).unwrap();
        crate::device::unregister(reg.id).unwrap();
        assert_eq!(
            temp_file.read(&mut [0u8; 8]).unwrap_err(),
            VfsError::NoDevice
        );
        drop(temp_file);
        assert!(
            gpu.wait_idle(resources_before, 2_000_000),
            "transient DRM session did not retire"
        );
    }
    // No global `pmm::stats().free` equality here: the user-mode self-test
    // task is still being reaped on another CPU while this runs, and releasing
    // its address space legitimately changes the global count by ~4 MiB.
    // GPU ownership is covered above by live_resources/session/host_ranges.

    {
        task.lock().address_space = old.clone();
    }
    if let Some(old) = old {
        old.lock().activate();
    } else {
        UserAddressSpace::deactivate();
    }
    drop(scratch);
    crate::verbose::log(format_args!("VirtIO GPU resource self-test passed"));
    crate::verbose::log(format_args!(
        "DRM ioctl/GEM/PRIME/syncobj/IRQ self-test passed"
    ));
    if verified_venus && verified_virgl {
        "virtio-gpu(kms+virgl+venus-abi+blob+reclaim)"
    } else if verified_venus {
        "virtio-gpu(kms+venus-abi+blob+reclaim)"
    } else if verified_virgl {
        "virtio-gpu(kms+virgl+reclaim)"
    } else {
        "virtio-gpu(kms+reclaim)"
    }
}
fn context(file: &Arc<dyn Inode>, capset: u32, space: &Arc<Mutex<UserAddressSpace>>) {
    let mut params = vec![0; 32];
    put64(&mut params, 0, 1);
    put64(&mut params, 8, capset as u64);
    put64(&mut params, 16, 2);
    put64(&mut params, 24, 2);
    space.lock().write_bytes(0x13000, &params).unwrap();
    let mut init = empty(16);
    put32(&mut init, 0, 2);
    put64(&mut init, 8, 0x13000);
    call(file, 0x4b, &mut init, space).unwrap();
}
fn virgl(gpu: &Arc<Gpu>, file: &Arc<dyn Inode>, space: &Arc<Mutex<UserAddressSpace>>) {
    let capset = gpu
        .capsets
        .lock()
        .iter()
        .find(|c| c.id == 2 || c.id == 1)
        .map(|c| c.id);
    let Some(capset) = capset else {
        return;
    };
    if gpu.transport.features & 16 != 0 {
        context(file, capset, space);
    }
    let mut create = empty(56);
    for (o, v) in [
        (0, 2),
        (4, 2),
        (8, 2),
        (12, 64),
        (16, 64),
        (20, 1),
        (24, 1),
        (48, 16384),
        (52, 256),
    ] {
        put32(&mut create, o, v);
    }
    call(file, 0x44, &mut create, space).unwrap();
    let handle = u32_at(&create, 40).unwrap();
    let resource = u32_at(&create, 44).unwrap();
    // The VirGL command words are fixed constants for this clear-only probe;
    // no shader compilation or floating point instructions run in the kernel.
    let words = [
        1 | (8 << 8) | (5 << 16),
        1,
        resource,
        2,
        0,
        0,
        5 | (3 << 16),
        1,
        0,
        1,
        7 | (8 << 16),
        4,
        0x3f800000,
        0,
        0,
        0x3f800000,
        0,
        0,
        0,
    ];
    let mut stream = Vec::new();
    for word in words {
        stream.extend_from_slice(&word.to_le_bytes());
    }
    space.lock().write_bytes(0x14000, &stream).unwrap();
    space
        .lock()
        .write_bytes(0x15000, &handle.to_le_bytes())
        .unwrap();
    let mut exec = empty(64);
    put32(&mut exec, 4, stream.len() as u32);
    put64(&mut exec, 8, 0x14000);
    put64(&mut exec, 16, 0x15000);
    put32(&mut exec, 24, 1);
    call(file, 0x42, &mut exec, space).unwrap();
    let mut wait = empty(8);
    put32(&mut wait, 0, handle);
    call(file, 0x48, &mut wait, space).unwrap();
    let mut transfer = empty(44);
    for (o, v) in [(0, handle), (16, 64), (20, 64), (24, 1), (36, 256)] {
        put32(&mut transfer, o, v);
    }
    call(file, 0x46, &mut transfer, space).unwrap();
    let mut map = empty(16);
    put32(&mut map, 8, handle);
    call(file, 0x41, &mut map, space).unwrap();
    file.mmap(
        &mut space.lock(),
        0x50000,
        16384,
        u64_at(&map, 0).unwrap(),
        3,
    )
    .unwrap();
    let mut pixel = [0; 4];
    space.lock().read_bytes(0x50000, &mut pixel).unwrap();
    assert_eq!(
        &pixel[..3],
        &[0, 0, 255],
        "VirGL clear readback did not produce red"
    );
    space.lock().unmap(0x50000, 16384).unwrap();
    crate::verbose::log(format_args!(
        "VirGL host-rendered clear/readback self-test passed"
    ));
}
fn venus(gpu: &Arc<Gpu>, space: &Arc<Mutex<UserAddressSpace>>) {
    let file = vfs::open_abs("/dev/dri/renderD128", vfs::abi::O_RDWR, 0)
        .unwrap()
        .inode
        .clone();
    // 1. Verify GETPARAM reports RESOURCE_BLOB(3), HOST_VISIBLE(4), CONTEXT_INIT(6)
    for param in [3u64, 4, 6, 7] {
        let mut gp = empty(16);
        put64(&mut gp, 0, param);
        put64(&mut gp, 8, 0x15800);
        call(&file, 0x43, &mut gp, space).unwrap();
        let mut val = [0u8; 8];
        space.lock().read_bytes(0x15800, &mut val).unwrap();
        assert_ne!(u64::from_le_bytes(val), 0, "Venus param {} zero", param);
    }
    // 2. Context-init with Venus capset (4) and 2 guest command rings.
    //    virtio-gpu CTX_CREATE carries only the capset id in context_init;
    //    the host binds ring N only when a Venus client calls
    //    vkGetDeviceQueue2(VkDeviceQueueTimelineInfoMESA.ringIdx = N).
    //    No Mesa/Venus userspace is ported, so the kernel test drives ring 0
    //    and treats the ring-1 host path as not verified here.
    let mut params = vec![0; 48];
    put64(&mut params, 0, 1);
    put64(&mut params, 8, 4);
    put64(&mut params, 16, 2);
    put64(&mut params, 24, 2);
    put64(&mut params, 32, 3);
    put64(&mut params, 40, 1);
    space.lock().write_bytes(0x13000, &params).unwrap();
    let mut init = empty(16);
    put32(&mut init, 0, 3);
    put64(&mut init, 8, 0x13000);
    call(&file, 0x4b, &mut init, space).unwrap();
    // 3. Query Venus capset
    let mut caps = empty(24);
    put32(&mut caps, 0, 4);
    put64(&mut caps, 8, 0x16000);
    put32(&mut caps, 16, 4096);
    call(&file, 0x49, &mut caps, space).unwrap();
    // 4. Create and map a HOST-VISIBLE blob (VIRTGPU_BLOB_MEM_HOST3D = 2, USE_MAPPABLE = 1)
    let mut create = empty(56);
    put32(&mut create, 0, 2);
    put32(&mut create, 4, 1);
    put64(&mut create, 16, 4096);
    call(&file, 0x4a, &mut create, space).unwrap();
    let handle = u32_at(&create, 8).unwrap();
    let mut info = empty(16);
    put32(&mut info, 0, handle);
    call(&file, 0x45, &mut info, space).unwrap();
    assert_eq!(u32_at(&info, 12).unwrap(), 2);
    let mut map = empty(16);
    put32(&mut map, 8, handle);
    call(&file, 0x41, &mut map, space).unwrap();
    file.mmap(
        &mut space.lock(),
        0x60000,
        4096,
        u64_at(&map, 0).unwrap(),
        3,
    )
    .unwrap();
    space
        .lock()
        .write_bytes(0x60000, b"Venus host-visible blob")
        .unwrap();
    let mut readback = [0u8; 23];
    space.lock().read_bytes(0x60000, &mut readback).unwrap();
    assert_eq!(&readback, b"Venus host-visible blob");
    space.lock().unmap(0x60000, 4096).unwrap();
    assert!(gpu.transport.host_visible.is_some());
    // 5. Submit on ring 0 with a timeline syncobj out-sync and verify the
    //    ring poll event.  Ring 1 is rejected locally: the kernel advertises
    //    two rings, but no host sync queue exists for ring 1 without a Venus
    //    client, and silently accepting an unbacked ring would only produce a
    //    host-side fence error.
    let mut invalid = empty(64);
    put32(&mut invalid, 0, 4); // VIRTGPU_EXECBUF_RING_IDX
    put32(&mut invalid, 32, 2); // ring_idx = 2, outside the 2-ring context
    assert_eq!(
        call(&file, 0x42, &mut invalid, space).unwrap_err(),
        VfsError::InvalidArgument
    );
    let mut sync = empty(8);
    call(&file, 0xbf, &mut sync, space).unwrap();
    let sync_handle = u32_at(&sync, 0).unwrap();
    let mut out_sync = [0u8; 16];
    put32(&mut out_sync, 0, sync_handle);
    put64(&mut out_sync, 8, 42);
    space.lock().write_bytes(0x16800, &out_sync).unwrap();
    space
        .lock()
        .write_bytes(0x16820, &handle.to_le_bytes())
        .unwrap();
    let mut exec = empty(64);
    put32(&mut exec, 0, 4); // VIRTGPU_EXECBUF_RING_IDX
    put32(&mut exec, 4, 0);
    put64(&mut exec, 16, 0x16820);
    put32(&mut exec, 24, 1);
    put32(&mut exec, 32, 0); // ring_idx = 0 (CPU/host timeline)
    put32(&mut exec, 36, 16); // syncobj_stride = 16
    put32(&mut exec, 44, 1); // num_out_syncobjs = 1
    put64(&mut exec, 56, 0x16800);
    call(&file, 0x42, &mut exec, space).unwrap();
    space
        .lock()
        .write_bytes(0x16840, &sync_handle.to_le_bytes())
        .unwrap();
    space
        .lock()
        .write_bytes(0x16850, &42u64.to_le_bytes())
        .unwrap();
    let mut twait = empty(48);
    put64(&mut twait, 0, 0x16840);
    put64(&mut twait, 8, 0x16850);
    put64(
        &mut twait,
        16,
        (crate::time::uptime_micros() + 1_000_000) * 1000,
    );
    put32(&mut twait, 24, 1);
    call(&file, 0xca, &mut twait, space).unwrap();
    // A syncobj wait reports readiness for both success and error fences.
    // Export the timeline point and inspect the sync_file poll state so a
    // host-rejected fence cannot masquerade as a passing Venus submission.
    let mut handle_to_fd = empty(24);
    put32(&mut handle_to_fd, 0, sync_handle);
    put32(&mut handle_to_fd, 4, 3); // EXPORT_SYNC_FILE | TIMELINE
    put64(&mut handle_to_fd, 16, 42);
    call(&file, 0xc1, &mut handle_to_fd, space).unwrap();
    let sync_fd = u32_at(&handle_to_fd, 8).unwrap();
    let sync_poll = fd_inode(sync_fd).unwrap().poll();
    assert!(sync_poll & 1 != 0, "Venus fence did not signal");
    assert_eq!(sync_poll & 8, 0, "Venus fence signalled an error");
    vfs::current_fd_table()
        .lock()
        .close(sync_fd as usize)
        .unwrap();
    assert!(file.poll() & 1 != 0);
    let mut ring_ev = [0u8; 8];
    assert_eq!(file.read_at(0, &mut ring_ev).unwrap(), 8);
    assert_eq!(u32_at(&ring_ev, 0).unwrap(), 0x90000000);
    crate::verbose::log(format_args!(
        "Venus capset/context/blob/ring-0 fence kernel self-test passed"
    ));
}

fn modeset(gpu: &Arc<Gpu>, file: &Arc<dyn Inode>, space: &Arc<Mutex<UserAddressSpace>>) {
    let output = gpu.outputs.lock()[0].clone();
    let mut dumb = empty(32);
    put32(&mut dumb, 0, output.height);
    put32(&mut dumb, 4, output.width);
    put32(&mut dumb, 8, 32);
    call(file, 0xb2, &mut dumb, space).unwrap();
    let handle = u32_at(&dumb, 16).unwrap();
    let mut fb = empty(104);
    for (o, v) in [
        (4, output.width),
        (8, output.height),
        (12, 0x34325258),
        (20, handle),
        (36, output.width * 4),
    ] {
        put32(&mut fb, o, v);
    }
    call(file, 0xb8, &mut fb, space).unwrap();
    let framebuffer = u32_at(&fb, 0).unwrap();
    let mut connector = empty(80);
    put64(&mut connector, 8, 0x18000);
    put32(&mut connector, 32, 1);
    put32(&mut connector, 48, 0x200);
    call(file, 0xa7, &mut connector, space).unwrap();
    let mut blob = empty(16);
    put64(&mut blob, 0, 0x18000);
    put32(&mut blob, 8, 68);
    call(file, 0xbd, &mut blob, space).unwrap();
    let mode = u32_at(&blob, 12).unwrap();
    let mut cap = empty(16);
    put64(&mut cap, 0, 3);
    put64(&mut cap, 8, 1);
    call(file, 0x0d, &mut cap, space).unwrap();
    let objects = [0x100u32, 0x200, 0x400];
    let counts = [3u32, 1, 10];
    let properties = [1u32, 2, 15, 3, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
    let values = [
        1u64,
        mode as u64,
        0x19000,
        0x100,
        0x100,
        framebuffer as u64,
        0,
        0,
        (output.width as u64) << 16,
        (output.height as u64) << 16,
        0,
        0,
        output.width as u64,
        output.height as u64,
    ];
    for (address, words) in [
        (0x18100, objects.as_slice()),
        (0x18140, counts.as_slice()),
        (0x18200, properties.as_slice()),
    ] {
        let mut bytes = Vec::new();
        for word in words {
            bytes.extend_from_slice(&word.to_le_bytes());
        }
        space.lock().write_bytes(address, &bytes).unwrap();
    }
    let mut bytes = Vec::new();
    for value in values {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    space.lock().write_bytes(0x18300, &bytes).unwrap();
    space
        .lock()
        .write_bytes(0x19000, &u32::MAX.to_le_bytes())
        .unwrap();
    let mut atomic = empty(56);
    put32(&mut atomic, 0, 0x500);
    put32(&mut atomic, 4, 3);
    for (o, p) in [(8, 0x18100), (16, 0x18140), (24, 0x18200), (32, 0x18300)] {
        put64(&mut atomic, o, p);
    }
    put64(&mut atomic, 48, 0x391);
    let before = gpu.displays.lock().generation;
    call(file, 0xbc, &mut atomic, space).unwrap();
    assert_eq!(gpu.displays.lock().generation, before);
    let mut fd = [0; 4];
    space.lock().read_bytes(0x19000, &mut fd).unwrap();
    assert_eq!(
        u32::from_le_bytes(fd),
        u32::MAX,
        "TEST_ONLY allocated a fence fd"
    );
    put32(&mut atomic, 0, 0x401);
    call(file, 0xbc, &mut atomic, space).unwrap();
    assert_eq!(gpu.displays.lock().generation, before + 1);
    space.lock().read_bytes(0x19000, &mut fd).unwrap();
    let fd = u32::from_le_bytes(fd);
    assert!(fd_inode(fd).unwrap().poll() & 1 != 0);
    vfs::current_fd_table().lock().close(fd as usize).unwrap();
    let mut event = [0; 32];
    assert_eq!(file.read_at(0, &mut event).unwrap(), 32);
    assert_eq!(u32_at(&event, 0).unwrap(), 2);
    assert_eq!(u64_at(&event, 8).unwrap(), 0x391);
    space
        .lock()
        .write_bytes(0x18300 + 12 * 8, &(output.width as u64 + 1).to_le_bytes())
        .unwrap();
    put32(&mut atomic, 0, 0x500);
    assert_eq!(
        call(file, 0xbc, &mut atomic, space).unwrap_err(),
        VfsError::InvalidArgument
    );
    assert_eq!(gpu.displays.lock().generation, before + 1);
    // Test cursor2 (0xbb) and cursor move (0xa3) on the cursor virtqueue
    let mut cur_dumb = empty(32);
    put32(&mut cur_dumb, 0, 64);
    put32(&mut cur_dumb, 4, 64);
    put32(&mut cur_dumb, 8, 32);
    call(file, 0xb2, &mut cur_dumb, space).unwrap();
    let cur_handle = u32_at(&cur_dumb, 16).unwrap();
    let mut cursor2 = empty(36);
    put32(&mut cursor2, 0, 1); // DRM_MODE_CURSOR_BO
    put32(&mut cursor2, 4, 0x100);
    put32(&mut cursor2, 8, 16);
    put32(&mut cursor2, 12, 16);
    put32(&mut cursor2, 16, 64);
    put32(&mut cursor2, 20, 64);
    put32(&mut cursor2, 24, cur_handle);
    call(file, 0xbb, &mut cursor2, space).unwrap();
    let mut cursor_move = empty(28);
    put32(&mut cursor_move, 0, 2); // DRM_MODE_CURSOR_MOVE
    put32(&mut cursor_move, 4, 0x100);
    put32(&mut cursor_move, 8, 32);
    put32(&mut cursor_move, 12, 32);
    call(file, 0xa3, &mut cursor_move, space).unwrap();
    crate::verbose::log(format_args!(
        "KMS atomic TEST_ONLY/commit/event/out-fence self-test passed"
    ));
}
