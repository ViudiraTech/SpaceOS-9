/*
 *
 *       src/drm/self_test.rs
 *       simpledrm end-to-end check through the Linux DRM ioctl ABI
 *
 *       2026/11/15 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! Loads simplefb.ko on the firmware framebuffer, drives the resulting
//! /dev/dri/cardN with the same ioctls a KMS client issues (capabilities,
//! resources, dumb buffers + mmap, legacy SETCRTC/DIRTYFB/PAGE_FLIP with an
//! event, atomic TEST_ONLY and FB_DAMAGE_CLIPS, WAIT_VBLANK) and reads the
//! scanout memory back to check what reached the screen, including format
//! conversion and that undamaged pixels are left alone. Closing the file must
//! return the screen to the console; unloading the module removes the node.

use super::ioctl as nr;
use crate::{
    sync::Mutex,
    user::UserAddressSpace,
    vfs::{self, Inode, VfsError},
};
use alloc::{format, sync::Arc, vec, vec::Vec};

#[cfg(feature = "modules")]
static SIMPLEFB: &[u8] = include_bytes!(env!("SPK_SIMPLEFB_KO"));

const ARG: u64 = 0x10000;
const SCRATCH: u64 = 0x11000;
const XRGB_MAP: u64 = 0x1000_0000;
const RGB565_MAP: u64 = 0x2000_0000;
const OBJ_PLANE: u32 = 0xeeee_eeee;

struct Client {
    file: Arc<dyn Inode>,
    space: Arc<Mutex<UserAddressSpace>>,
}

fn put32(b: &mut [u8], o: usize, v: u32) {
    b[o..o + 4].copy_from_slice(&v.to_le_bytes());
}
fn put64(b: &mut [u8], o: usize, v: u64) {
    b[o..o + 8].copy_from_slice(&v.to_le_bytes());
}
fn at32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
fn at64(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

impl Client {
    fn call(&self, number: u8, bytes: &mut [u8]) -> Result<(), VfsError> {
        self.space.lock().write_bytes(ARG, bytes)?;
        let (size, dir) = nr::descriptor(number).unwrap();
        let cmd = ((dir as u64) << 30) | ((size as u64) << 16) | ((b'd' as u64) << 8) | number as u64;
        self.file.ioctl(cmd, ARG)?;
        self.space.lock().read_bytes(ARG, bytes)?;
        Ok(())
    }

    fn ioctl(&self, number: u8, fill: impl FnOnce(&mut [u8])) -> Vec<u8> {
        let mut bytes = vec![0u8; nr::descriptor(number).unwrap().0];
        fill(&mut bytes);
        self.call(number, &mut bytes)
            .unwrap_or_else(|e| panic!("simpledrm self-test: ioctl {number:#x} failed: {e:?}"));
        bytes
    }

    fn poke(&self, address: u64, bytes: &[u8]) {
        self.space.lock().write_bytes(address, bytes).unwrap();
    }

    fn peek(&self, address: u64, length: usize) -> Vec<u8> {
        let mut bytes = vec![0u8; length];
        self.space.lock().read_bytes(address, &mut bytes).unwrap();
        bytes
    }

    fn cap(&self, cap: u64) -> u64 {
        at64(&self.ioctl(nr::GET_CAP, |b| put64(b, 0, cap)), 8)
    }

    /// `(id, value)` of the property called `name` on `object`.
    fn property(&self, object: u32, kind: u32, name: &str) -> Option<(u32, u64)> {
        let count = at32(
            &self.ioctl(nr::MODE_OBJ_GETPROPERTIES, |b| {
                put32(b, 20, object);
                put32(b, 24, kind);
            }),
            16,
        ) as usize;
        self.ioctl(nr::MODE_OBJ_GETPROPERTIES, |b| {
            put64(b, 0, SCRATCH);
            put64(b, 8, SCRATCH + 0x400);
            put32(b, 16, count as u32);
            put32(b, 20, object);
            put32(b, 24, kind);
        });
        let ids = self.peek(SCRATCH, count * 4);
        let values = self.peek(SCRATCH + 0x400, count * 8);
        (0..count).find_map(|i| {
            let id = at32(&ids, 4 * i);
            let info = self.ioctl(nr::MODE_GETPROPERTY, |b| put32(b, 16, id));
            let text = &info[24..56];
            let len = text.iter().position(|&c| c == 0).unwrap_or(32);
            (&text[..len] == name.as_bytes()).then(|| (id, at64(&values, 8 * i)))
        })
    }

    fn create_blob(&self, data: &[u8]) -> u32 {
        self.poke(SCRATCH + 0x800, data);
        at32(
            &self.ioctl(nr::MODE_CREATEPROPBLOB, |b| {
                put64(b, 0, SCRATCH + 0x800);
                put32(b, 8, data.len() as u32);
            }),
            12,
        )
    }

    /// One-object atomic commit.
    fn atomic(&self, flags: u32, object: u32, props: &[(u32, u64)]) -> Result<(), VfsError> {
        let base = SCRATCH + 0xa00;
        self.poke(base, &object.to_le_bytes());
        self.poke(base + 8, &(props.len() as u32).to_le_bytes());
        for (i, (id, value)) in props.iter().enumerate() {
            self.poke(base + 16 + 4 * i as u64, &id.to_le_bytes());
            self.poke(base + 128 + 8 * i as u64, &value.to_le_bytes());
        }
        let mut bytes = vec![0u8; 56];
        put32(&mut bytes, 0, flags);
        put32(&mut bytes, 4, 1);
        put64(&mut bytes, 8, base);
        put64(&mut bytes, 16, base + 8);
        put64(&mut bytes, 24, base + 16);
        put64(&mut bytes, 32, base + 128);
        self.call(nr::MODE_ATOMIC, &mut bytes)
    }

    /// Create a dumb buffer and map it at `address`; returns (handle, pitch).
    fn dumb(&self, width: u32, height: u32, bpp: u32, address: u64) -> (u32, u32) {
        let dumb = self.ioctl(nr::MODE_CREATE_DUMB, |b| {
            put32(b, 0, height);
            put32(b, 4, width);
            put32(b, 8, bpp);
        });
        let (handle, pitch, size) = (at32(&dumb, 16), at32(&dumb, 20), at64(&dumb, 24));
        assert!(pitch >= width * bpp / 8 && size >= pitch as u64 * height as u64);
        let offset = at64(&self.ioctl(nr::MODE_MAP_DUMB, |b| put32(b, 0, handle)), 8);
        self.file
            .mmap(&mut self.space.lock(), address, size.next_multiple_of(4096), offset, 3)
            .expect("simpledrm self-test: dumb mmap");
        (handle, pitch)
    }

    fn addfb2(&self, width: u32, height: u32, format: u32, handle: u32, pitch: u32) -> u32 {
        at32(
            &self.ioctl(nr::MODE_ADDFB2, |b| {
                put32(b, 4, width);
                put32(b, 8, height);
                put32(b, 12, format);
                put32(b, 20, handle);
                put32(b, 36, pitch);
            }),
            0,
        )
    }

    fn fill(&self, address: u64, pitch: u32, rect: (u32, u32, u32, u32), cpp: u32, pixel: impl Fn(u32, u32) -> u32) {
        let (x0, y0, x1, y1) = rect;
        let mut row = vec![0u8; ((x1 - x0) * cpp) as usize];
        for y in y0..y1 {
            for x in x0..x1 {
                let at = ((x - x0) * cpp) as usize;
                row[at..at + cpp as usize].copy_from_slice(&pixel(x, y).to_le_bytes()[..cpp as usize]);
            }
            self.poke(address + y as u64 * pitch as u64 + (x0 * cpp) as u64, &row);
        }
    }
}

fn pattern(x: u32, y: u32) -> u32 {
    (x & 0xff) << 16 | (y & 0xff) << 8 | ((x ^ y) * 7 & 0xff)
}

struct Scanout {
    base: *const u8,
    pitch: usize,
}

impl Scanout {
    fn pixel(&self, x: u32, y: u32) -> u32 {
        // SAFETY: inside the boot framebuffer mapping (checked by callers'
        // coordinates against the mode, which equals the framebuffer size).
        unsafe {
            (self.base.add(y as usize * self.pitch + x as usize * 4) as *const u32).read_volatile() & 0x00ff_ffff
        }
    }
}

/// Run the test; returns the status for the SELFTEST_OK line.
pub fn run() -> &'static str {
    #[cfg(not(feature = "modules"))]
    {
        "off"
    }
    #[cfg(feature = "modules")]
    {
        if crate::boot::init_path().is_some() || SIMPLEFB.is_empty() {
            return "off";
        }
        if crate::gpu::device().is_some() {
            return "skip(native-gpu)";
        }
        let Some(fb) = crate::boot::framebuffer() else {
            return "none";
        };
        crate::module::load(SIMPLEFB, "").expect("simplefb module failed to load");
        let device = super::devices()
            .into_iter()
            .find(|d| d.config.name == "simpledrm")
            .expect("simplefb did not register a DRM device");
        let xrgb_native = fb.bpp == 32
            && (fb.red_mask_size, fb.red_mask_shift, fb.green_mask_shift, fb.blue_mask_shift) == (8, 16, 8, 0);
        let status = exercise(&device, fb, xrgb_native);
        let minor = device.minor;
        drop(device);
        #[cfg(feature = "module-unload")]
        {
            crate::module::unload("simplefb", false).expect("simplefb failed to unload");
            assert!(super::devices().iter().all(|d| d.config.name != "simpledrm"));
            assert!(
                vfs::open_abs(&format!("/dev/dri/card{minor}"), vfs::abi::O_RDWR, 0).is_err(),
                "card{minor} still present after unloading simplefb"
            );
        }
        let _ = minor;
        crate::kinfo!("DRM_SIMPLEFB_OK {}", status);
        "simpledrm"
    }
}

fn exercise(
    device: &Arc<super::Device>,
    fb: &limine::framebuffer::Framebuffer,
    xrgb_native: bool,
) -> alloc::string::String {
    let task = crate::sched::current().unwrap();
    let old = task.address_space();
    let space = Arc::new(Mutex::new(UserAddressSpace::new().unwrap()));
    space.lock().map_zeroed(ARG, 0x10000, 3).unwrap();
    task.lock().set_address_space(space.clone());
    let path = format!("/dev/dri/card{}", device.minor);
    let client = Client {
        file: vfs::open_abs(&path, vfs::abi::O_RDWR, 0).unwrap().inode.clone(),
        space,
    };
    let screen = Scanout { base: fb.address().cast_const().cast(), pitch: fb.pitch as usize };

    // Identification and capabilities.
    let version = client.ioctl(nr::VERSION, |b| {
        put64(b, 16, 32);
        put64(b, 24, SCRATCH);
    });
    assert_eq!(at64(&version, 16), 9);
    assert_eq!(client.peek(SCRATCH, 9), b"simpledrm");
    assert_eq!(client.cap(1), 1, "DRM_CAP_DUMB_BUFFER");
    assert_eq!(client.cap(4), 1, "DRM_CAP_DUMB_PREFER_SHADOW");
    assert_eq!(client.cap(6), 1, "DRM_CAP_TIMESTAMP_MONOTONIC");

    // Resources: one connector -> encoder -> CRTC with the firmware mode.
    let res = client.ioctl(nr::MODE_GETRESOURCES, |b| {
        put64(b, 8, SCRATCH);
        put64(b, 16, SCRATCH + 0x10);
        put32(b, 36, 1);
        put32(b, 40, 1);
    });
    assert_eq!((at32(&res, 36), at32(&res, 40), at32(&res, 44)), (1, 1, 1));
    let crtc = u32::from_le_bytes(client.peek(SCRATCH, 4).try_into().unwrap());
    let connector = u32::from_le_bytes(client.peek(SCRATCH + 0x10, 4).try_into().unwrap());
    let info = client.ioctl(nr::MODE_GETCONNECTOR, |b| {
        put64(b, 8, SCRATCH + 0x100);
        put32(b, 32, 1);
        put32(b, 48, connector);
    });
    assert_eq!((at32(&info, 32), at32(&info, 60)), (1, 1), "one mode, connected");
    let mode = client.peek(SCRATCH + 0x100, 68);
    let width = u16::from_le_bytes([mode[4], mode[5]]) as u32;
    let height = u16::from_le_bytes([mode[14], mode[15]]) as u32;
    assert_eq!((width as u64, height as u64), (fb.width, fb.height));
    assert!(width >= 320 && height >= 80);

    // Legacy modeset with an XRGB8888 dumb buffer.
    let (handle, pitch) = client.dumb(width, height, 32, XRGB_MAP);
    client.fill(XRGB_MAP, pitch, (0, 0, width, height), 4, pattern);
    let xrgb = client.addfb2(width, height, super::fourcc::XRGB8888, handle, pitch);
    client.poke(SCRATCH + 0x200, &connector.to_le_bytes());
    let setcrtc = |fb_id: u32| {
        client.ioctl(nr::MODE_SETCRTC, |b| {
            put64(b, 0, SCRATCH + 0x200);
            put32(b, 8, 1);
            put32(b, 12, crtc);
            put32(b, 16, fb_id);
            put32(b, 32, 1);
            b[36..104].copy_from_slice(&mode);
        });
    };
    setcrtc(xrgb);
    assert!(crate::tty::vt::scanout_claimed(), "SETCRTC did not take the console's scanout");
    let corners = [(0, 0), (width - 1, 0), (width / 2, height / 2), (width - 1, height - 1)];
    if xrgb_native {
        for (x, y) in corners {
            assert_eq!(screen.pixel(x, y), pattern(x, y), "modeset pixel ({x},{y})");
        }
    }

    // DIRTYFB: only the clip is presented.
    client.fill(XRGB_MAP, pitch, (8, 8, 40, 24), 4, |_, _| 0x00ff_0000);
    client.fill(XRGB_MAP, pitch, (200, 8, 232, 24), 4, |_, _| 0x0000_00ff);
    client.poke(SCRATCH + 0x300, &[8, 0, 8, 0, 40, 0, 24, 0]);
    client.ioctl(nr::MODE_DIRTYFB, |b| {
        put32(b, 0, xrgb);
        put32(b, 12, 1);
        put64(b, 16, SCRATCH + 0x300);
    });
    if xrgb_native {
        assert_eq!(screen.pixel(20, 10), 0x00ff_0000, "DIRTYFB clip not presented");
        assert_eq!(screen.pixel(210, 10), pattern(210, 10), "DIRTYFB touched pixels outside the clip");
    }

    // Format conversion: RGB565 is emulated on an XRGB8888 scanout.
    let mut converted = "native";
    if xrgb_native {
        let (handle565, pitch565) = client.dumb(width, height, 16, RGB565_MAP);
        client.fill(RGB565_MAP, pitch565, (0, 0, width, height), 2, |x, _| match x % 3 {
            0 => 0xf800,
            1 => 0x07e0,
            _ => 0x8410,
        });
        let fb565 = client.addfb2(width, height, super::fourcc::RGB565, handle565, pitch565);
        setcrtc(fb565);
        assert_eq!(screen.pixel(0, 5), 0x00ff_0000, "RGB565 red");
        assert_eq!(screen.pixel(1, 5), 0x0000_ff00, "RGB565 green");
        assert_eq!(screen.pixel(2, height - 1), 0x0084_8284, "RGB565 grey with bit replication");
        setcrtc(xrgb);
        client.ioctl(nr::MODE_RMFB, |b| put32(b, 0, fb565));
        converted = "rgb565->xrgb8888";
    }

    // PAGE_FLIP with an event (second XRGB buffer).
    let (handle2, pitch2) = client.dumb(width, height, 32, XRGB_MAP + 0x0800_0000);
    client.fill(XRGB_MAP + 0x0800_0000, pitch2, (0, 0, width, height), 4, |x, y| pattern(y, x));
    let flip_fb = client.addfb2(width, height, super::fourcc::XRGB8888, handle2, pitch2);
    client.ioctl(nr::MODE_PAGE_FLIP, |b| {
        put32(b, 0, crtc);
        put32(b, 4, flip_fb);
        put32(b, 8, 1);
        put64(b, 16, 0x5ced);
    });
    let mut event = [0u8; 32];
    assert_eq!(client.file.read_at(0, &mut event).unwrap(), 32);
    assert_eq!((at32(&event, 0), at64(&event, 8)), (2, 0x5ced), "flip-complete event");
    if xrgb_native {
        assert_eq!(screen.pixel(30, 70), pattern(70, 30), "page flip not presented");
    }

    // Atomic: plane lookup by property name, TEST_ONLY, damage clips.
    client.ioctl(nr::SET_CLIENT_CAP, |b| {
        put64(b, 0, 3);
        put64(b, 8, 1);
    });
    let planes = client.ioctl(nr::MODE_GETPLANERESOURCES, |b| {
        put64(b, 0, SCRATCH);
        put32(b, 8, 4);
    });
    let plane_count = at32(&planes, 8) as usize;
    let plane_ids = client.peek(SCRATCH, 4 * plane_count);
    let primary = (0..plane_count)
        .map(|i| at32(&plane_ids, 4 * i))
        .find(|&id| client.property(id, OBJ_PLANE, "type").map(|p| p.1) == Some(1))
        .expect("no primary plane");
    let (fb_prop, _) = client.property(primary, OBJ_PLANE, "FB_ID").unwrap();
    let (damage_prop, _) = client.property(primary, OBJ_PLANE, "FB_DAMAGE_CLIPS").unwrap();
    let (_, in_formats) = client.property(primary, OBJ_PLANE, "IN_FORMATS").unwrap();
    let blob = client.ioctl(nr::MODE_GETPROPBLOB, |b| put32(b, 0, in_formats as u32));
    assert!(at32(&blob, 4) >= 24, "IN_FORMATS blob");
    client
        .atomic(0x100, primary, &[(fb_prop, xrgb as u64)])
        .expect("atomic TEST_ONLY");
    assert!(
        client.atomic(0x100, primary, &[(fb_prop, 0xdead)]).is_err(),
        "TEST_ONLY accepted a bad framebuffer"
    );
    if xrgb_native {
        assert_eq!(screen.pixel(30, 70), pattern(70, 30), "TEST_ONLY changed the screen");
    }
    client.atomic(0, primary, &[(fb_prop, xrgb as u64)]).expect("atomic flip");
    // Redraw two squares; damage only the first.
    client.fill(XRGB_MAP, pitch, (64, 40, 96, 72), 4, |_, _| 0x0000_ff00);
    client.fill(XRGB_MAP, pitch, (160, 40, 192, 72), 4, |_, _| 0x00ff_00ff);
    let rect: Vec<u8> = [64i32, 40, 96, 72].iter().flat_map(|v| v.to_le_bytes()).collect();
    let clips = client.create_blob(&rect);
    client
        .atomic(0, primary, &[(fb_prop, xrgb as u64), (damage_prop, clips as u64)])
        .expect("atomic damage commit");
    if xrgb_native {
        assert_eq!(screen.pixel(70, 50), 0x0000_ff00, "damaged square not presented");
        assert_eq!(screen.pixel(170, 50), pattern(170, 50), "undamaged square presented");
    }
    // Damage without clips covers the plane.
    client.ioctl(nr::MODE_DIRTYFB, |b| put32(b, 0, xrgb));
    if xrgb_native {
        assert_eq!(screen.pixel(170, 50), 0x00ff_00ff, "full DIRTYFB");
    }

    // Simulated vblank advances at the mode's refresh rate.
    let first = at32(&client.ioctl(nr::WAIT_VBLANK, |b| put32(b, 0, 1)), 4);
    let second = at32(
        &client.ioctl(nr::WAIT_VBLANK, |b| {
            put32(b, 0, 1);
            put32(b, 4, 2);
        }),
        4,
    );
    assert!(second >= first + 2, "vblank counter {first} -> {second}");

    // Last close hands the screen back to the console.
    drop(client);
    assert!(!crate::tty::vt::scanout_claimed(), "lastclose did not release the console");

    task.lock().address_space = old.clone();
    if let Some(old) = old {
        old.lock().activate();
    } else {
        UserAddressSpace::deactivate();
    }
    format!(
        "{}x{} bpp={} pitch={} convert={} damage=ok flip=ok vblank=ok",
        width, height, fb.bpp, fb.pitch, converted
    )
}
