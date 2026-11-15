/*
 *
 *       src/drm/virtio/display.rs
 *       virtio-gpu scanouts, damage transfer and the hardware cursor
 *
 *       2026/10/24 By JiTianYu391
 *       Copyright (C) 2026 ViudiraTech.
 *
 */

//! The DRM core hands this driver validated plane states; programming one
//! is (Linux `virtio_gpu_primary_plane_update`):
//!
//! 1. wait for the GPU work that last wrote the framebuffer (implicit sync);
//! 2. TRANSFER_TO_HOST_2D the damaged rectangles of guest-backed resources;
//! 3. SET_SCANOUT (or SET_SCANOUT_BLOB) when the scanout changed;
//! 4. RESOURCE_FLUSH the damage so the host presents it.
//!
//! The cursor plane uses the cursor queue (UPDATE_CURSOR / MOVE_CURSOR).

use super::{Gpu, ResourceObject, VirtFile, command, u32_at};
use crate::{
    drm::{
        self, ConnectorType, CursorUpdate, DriverFile, File, Framebuffer, GemBacking, Mode,
        PipeConfig, PlaneUpdate, Rect, fourcc,
    },
    vfs::VfsError,
};
use alloc::{sync::Arc, vec::Vec};
use core::sync::atomic::Ordering;

/// Formats the primary plane scans out (VIRTIO_GPU_FORMAT_B8G8R8X8 / A8).
const FORMATS: [u32; 2] = [fourcc::XRGB8888, fourcc::ARGB8888];
const CURSOR_SIZE: u32 = 64;

#[derive(Clone)]
pub struct Output {
    pub width: u32,
    pub height: u32,
    pub connected: bool,
    pub edid: Vec<u8>,
}

/// What a scanout currently shows.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Scanout {
    resource: u32,
    source: Rect,
    pitch: u32,
    offset: u32,
    format: u32,
}

/// GET_DISPLAY_INFO and, with VIRTIO_GPU_F_EDID, each scanout's EDID.
pub fn discover(gpu: &Arc<Gpu>) -> Result<Vec<Output>, VfsError> {
    let count = gpu.transport.config32(8).min(16) as usize;
    if count == 0 {
        return Err(VfsError::NoDevice);
    }
    let reply = gpu.call(0x100, 0, &[], 24 + 16 * 24)?;
    let mut outputs = Vec::new();
    for index in 0..count {
        let o = 24 + index * 24;
        let mut width = u32_at(&reply, o + 8)?;
        let mut height = u32_at(&reply, o + 12)?;
        let connected = u32_at(&reply, o + 16)? != 0;
        if width == 0 || width > 8192 {
            width = 1024;
        }
        if height == 0 || height > 8192 {
            height = 768;
        }
        let edid = if gpu.transport.features & 2 != 0 {
            match gpu.call(0x10a, 0, &[index as u32, 0], 1056) {
                Ok(reply) => {
                    let size = u32_at(&reply, 24)? as usize;
                    if size > 1024 || size % 128 != 0 || 32 + size > reply.len() {
                        return Err(VfsError::IoError);
                    }
                    reply[32..32 + size].to_vec()
                }
                Err(_) => Vec::new(),
            }
        } else {
            Vec::new()
        };
        outputs.push(Output {
            width,
            height,
            connected,
            edid,
        });
    }
    Ok(outputs)
}

/// One DRM pipe per scanout: a virtual connector with the host's preferred
/// size, a primary plane and a 64x64 cursor plane.
pub fn pipes(outputs: &[Output]) -> Vec<PipeConfig> {
    outputs
        .iter()
        .map(|output| PipeConfig {
            connector_type: ConnectorType::Virtual,
            connected: output.connected,
            modes: alloc::vec![Mode::fixed(output.width as u16, output.height as u16, 60)],
            width_mm: 0,
            height_mm: 0,
            edid: output.edid.clone(),
            formats: FORMATS.to_vec(),
            cursor: Some((CURSOR_SIZE, CURSOR_SIZE)),
        })
        .collect()
}

fn resource_of(fb: &Framebuffer) -> Result<Arc<super::Resource>, VfsError> {
    fb.object
        .backing::<ResourceObject>()
        .map(|r| r.0.clone())
        .ok_or(VfsError::InvalidArgument)
}

impl Gpu {
    /// Transfer `clips` (framebuffer pixels) of a guest-backed resource to
    /// the host and flush their union. Many clips become one transfer of the
    /// bounding box; each RESOURCE_FLUSH makes the host present again, so a
    /// single flush matters more than the area.
    fn flush(
        &self,
        fb: &Framebuffer,
        resource: &super::Resource,
        clips: &[Rect],
    ) -> Result<(), VfsError> {
        let bounds = Rect::new(0, 0, fb.width, fb.height);
        let clips: Vec<Rect> = clips.iter().filter_map(|c| c.intersect(&bounds)).collect();
        let Some(union) = clips.iter().copied().reduce(|a, b| a.union(&b)) else {
            return Ok(());
        };
        let transfers: &[Rect] = if clips.len() > 32 {
            core::slice::from_ref(&union)
        } else {
            &clips
        };
        let mut pending = Vec::with_capacity(transfers.len());
        for clip in transfers {
            let offset = (clip.y1 as u64)
                .checked_mul(fb.pitch as u64)
                .and_then(|value| value.checked_add(clip.x1 as u64 * 4))
                .ok_or(VfsError::InvalidArgument)?;
            let words = [
                clip.x1 as u32,
                clip.y1 as u32,
                (clip.x2 - clip.x1) as u32,
                (clip.y2 - clip.y1) as u32,
                offset as u32,
                (offset >> 32) as u32,
                resource.id,
                0,
            ];
            match self.submit_call(0x105, 0, &words, 24) {
                Ok(request) => pending.push(request),
                Err(VfsError::WouldBlock) if !pending.is_empty() => {
                    for request in pending.drain(..) {
                        self.transport.wait(&request)?;
                    }
                    pending.push(self.submit_call(0x105, 0, &words, 24)?);
                }
                Err(error) => {
                    for request in pending {
                        let _ = self.transport.wait(&request);
                    }
                    return Err(error);
                }
            }
        }
        for request in pending {
            self.transport.wait(&request)?;
        }
        if resource.blob_mem == 0 {
            self.call(
                0x104,
                0,
                &[
                    union.x1 as u32,
                    union.y1 as u32,
                    (union.x2 - union.x1) as u32,
                    (union.y2 - union.y1) as u32,
                    resource.id,
                    0,
                ],
                24,
            )?;
        }
        Ok(())
    }

    fn disable_scanout(&self, pipe: usize) -> Result<(), VfsError> {
        self.scanouts.lock()[pipe] = None;
        self.call(0x103, 0, &[0, 0, 0, 0, pipe as u32, 0], 24)
            .map(|_| ())
    }

    fn take_display(&self) {
        self.display_owned.store(true, Ordering::Release);
        self.console_visible.store(false, Ordering::Release);
    }
}

impl drm::Driver for Gpu {
    fn set_mode(&self, pipe: usize, mode: Option<&Mode>) -> Result<(), VfsError> {
        match mode {
            // The scanout is programmed with its framebuffer in update_plane.
            Some(_) => {
                self.take_display();
                Ok(())
            }
            None => self.disable_scanout(pipe),
        }
    }

    fn update_plane(&self, pipe: usize, update: &PlaneUpdate) -> Result<(), VfsError> {
        let Some(fb) = update.framebuffer else {
            return self.disable_scanout(pipe);
        };
        let resource = resource_of(fb)?;
        // Implicit sync: present only what the GPU finished rendering.
        let last = resource.last.lock().clone();
        if let Some(fence) = last {
            fence.wait()?;
        }
        self.take_display();
        let source = update.source;
        let scanout = Scanout {
            resource: resource.id,
            source,
            pitch: fb.pitch,
            offset: fb.offset,
            format: fb.format,
        };
        if self.scanouts.lock()[pipe] == Some(scanout) {
            return self.flush(fb, &resource, update.damage);
        }
        // New image or position: upload it whole, then point the scanout
        // at it.
        self.flush(fb, &resource, &[Rect::new(0, 0, fb.width, fb.height)])?;
        let (x, y) = (source.x1 as u32, source.y1 as u32);
        let (w, h) = (
            (source.x2 - source.x1) as u32,
            (source.y2 - source.y1) as u32,
        );
        if resource.blob_mem != 0 {
            let format = if fb.format == fourcc::XRGB8888 { 2 } else { 1 };
            let words = [
                x,
                y,
                w,
                h,
                pipe as u32,
                resource.id,
                fb.width,
                fb.height,
                format,
                0,
                fb.pitch,
                0,
                0,
                0,
                fb.offset,
                0,
                0,
                0,
            ];
            self.call(0x10d, 0, &words, 24)?;
        } else {
            self.call(0x103, 0, &[x, y, w, h, pipe as u32, resource.id], 24)?;
            self.call(0x104, 0, &[x, y, w, h, resource.id, 0], 24)?;
        }
        self.scanouts.lock()[pipe] = Some(scanout);
        Ok(())
    }

    fn update_cursor(&self, pipe: usize, update: &CursorUpdate) -> Result<(), VfsError> {
        let (x, y) = (update.x as u32, update.y as u32);
        let image = update.framebuffer.map_or(0, |fb| fb.id);
        if image != 0 && self.cursors.lock()[pipe] == image {
            // Same image: MOVE_CURSOR only.
            let cmd = command(0x0301, 0, &[pipe as u32, x, y, 0, 0, 0, 0, 0]);
            let request = self.transport.submit(true, &cmd, 24, Vec::new())?;
            self.transport.wait(&request)?;
            return Ok(());
        }
        let resource_id = match update.framebuffer {
            Some(fb) => {
                let resource = resource_of(fb)?;
                if resource.width != CURSOR_SIZE || resource.height != CURSOR_SIZE {
                    return Err(VfsError::InvalidArgument);
                }
                self.call(
                    0x105,
                    0,
                    &[0, 0, CURSOR_SIZE, CURSOR_SIZE, 0, 0, resource.id, 0],
                    24,
                )?;
                resource.id
            }
            None => 0,
        };
        let cmd = command(
            0x0300,
            0,
            &[
                pipe as u32,
                x,
                y,
                0,
                resource_id,
                update.hot_x as u32,
                update.hot_y as u32,
                0,
            ],
        );
        let request = self.transport.submit(true, &cmd, 24, Vec::new())?;
        self.transport.wait(&request)?;
        self.cursors.lock()[pipe] = image;
        Ok(())
    }

    fn lastclose(&self) {
        // Give the scanout back to the text console: the service thread
        // re-points it on its next tick.
        for scanout in self.scanouts.lock().iter_mut() {
            *scanout = None;
        }
        for cursor in self.cursors.lock().iter_mut() {
            *cursor = 0;
        }
        self.display_owned.store(false, Ordering::Release);
        self.console_visible.store(false, Ordering::Release);
        drm::syncobj::fence_event();
    }

    fn create_dumb(
        &self,
        width: u32,
        height: u32,
        bpp: u32,
    ) -> Result<(Arc<dyn GemBacking>, u32), VfsError> {
        if bpp != 32 {
            return Err(VfsError::InvalidArgument);
        }
        let gpu = super::device().ok_or(VfsError::NoDevice)?;
        let resource = gpu.create_2d(width, height)?;
        let pitch = resource.stride;
        Ok((Arc::new(ResourceObject(resource)), pitch))
    }

    fn check_framebuffer(&self, fb: &Framebuffer) -> Result<(), VfsError> {
        let resource = resource_of(fb)?;
        if fb.format == fourcc::ARGB8888 && fb.width <= CURSOR_SIZE && fb.height <= CURSOR_SIZE {
            return Ok(()); // possible cursor image
        }
        if !FORMATS.contains(&fb.format) {
            return Err(VfsError::InvalidArgument);
        }
        // A guest-backed 2D resource is transferred as a whole: the
        // framebuffer must describe exactly its layout.
        if resource.blob_mem == 0
            && (resource.width != fb.width || fb.pitch != resource.stride || fb.offset != 0)
        {
            return Err(VfsError::InvalidArgument);
        }
        Ok(())
    }

    fn ioctl_descriptor(&self, nr: u8) -> Option<(usize, u32)> {
        super::ioctl_descriptor(nr)
    }

    fn open(&self, file: &Arc<File>) -> Result<Option<Arc<dyn DriverFile>>, VfsError> {
        let gpu = super::device().ok_or(VfsError::NoDevice)?;
        let private: Arc<VirtFile> = gpu.open_file(file);
        Ok(Some(private))
    }

    fn kick(&self) {
        self.transport.poll();
    }
}
