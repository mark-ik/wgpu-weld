// Copyright 2026 Mark Alan Boykin
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0

//! Linux: CEF DMABUF policy -> Graft -> Vulkan/wgpu.
//!
//! Welding decides how to handle CEF's implicit modifier. Graft owns explicit
//! DRM-modifier image creation, memory import, fd lifetime, compatible memory
//! selection, shared-fd planes, and foreign-queue acquisition.

use super::*;
use std::os::fd::{FromRawFd, IntoRawFd, OwnedFd, RawFd};

/// `DRM_FORMAT_MOD_INVALID` from `drm_fourcc.h`: no explicit modifier.
const DRM_FORMAT_MOD_INVALID: u64 = 0x00ff_ffff_ffff_ffff;

/// `DRM_FORMAT_MOD_LINEAR` from `drm_fourcc.h`: no tiling, row-major.
const DRM_FORMAT_MOD_LINEAR: u64 = 0;

impl DmaBufImage {
    /// Build an owned DMABUF image from an owned descriptor buffer table and
    /// copyable per-plane metadata.
    ///
    /// The image owns every descriptor from this point onward. On validation
    /// failure, the supplied `OwnedFd`s are dropped and closed.
    pub fn from_owned_buffers(
        buffers: Vec<OwnedFd>,
        planes: Vec<DmaBufPlane>,
        size: PhysicalSize<u32>,
        format: wgpu::TextureFormat,
        drm_format: u32,
        modifier: u64,
        generation: u64,
    ) -> Result<Self, ImportError> {
        validate_image_layout(buffers.len(), &planes)?;
        Ok(Self {
            buffers,
            planes,
            size,
            format,
            drm_format,
            modifier,
            generation,
        })
    }

    /// Build an owned DMABUF image from owned raw per-plane descriptors.
    ///
    /// Each tuple is `(owned_fd, offset, size, stride)`. Repeated numeric fd
    /// values are treated as repeated references to one descriptor owner before
    /// `OwnedFd` is constructed. Distinct dup fds for the same kernel object
    /// are deduplicated by identity and the duplicate descriptor is closed
    /// during construction.
    ///
    /// # Safety
    ///
    /// Every non-negative raw fd must be uniquely owned by the caller for this
    /// handoff. After this call, ownership belongs to the returned
    /// `DmaBufImage`, or all accepted descriptors are closed if construction
    /// fails.
    pub unsafe fn from_owned_raw_planes(
        raw_planes: Vec<(RawFd, u64, u64, u32)>,
        size: PhysicalSize<u32>,
        format: wgpu::TextureFormat,
        drm_format: u32,
        modifier: u64,
        generation: u64,
    ) -> Result<Self, ImportError> {
        let (buffers, planes) = deduplicated_raw_planes(raw_planes)?;
        Self::from_owned_buffers(
            buffers, planes, size, format, drm_format, modifier, generation,
        )
    }

    pub fn buffer_count(&self) -> usize {
        self.buffers.len()
    }
}

/// Construct the unified host device with Graft's complete Linux DMA-BUF
/// extension set, including `VK_EXT_queue_family_foreign`.
pub fn build_dmabuf_capable_device(
    adapter: &wgpu::Adapter,
    desc: &wgpu::DeviceDescriptor<'_>,
) -> Result<(wgpu::Device, wgpu::Queue), ImportError> {
    let host = grafting::vulkan_dmabuf::create_dmabuf_host_context(adapter, desc)
        .map_err(|error| ImportError::VulkanImport(error.to_string()))?;
    Ok((host.device, host.queue))
}

/// Can the host import a buffer whose modifier CEF left implicit?
#[cfg(feature = "wgpu-30")]
fn host_can_import_implicit_modifier(device: &wgpu::Device) -> bool {
    device
        .features()
        .contains(wgpu::Features::VULKAN_EXTERNAL_MEMORY_DMA_BUF)
}

#[cfg(not(feature = "wgpu-30"))]
fn host_can_import_implicit_modifier(_device: &wgpu::Device) -> bool {
    false
}

/// The Linux half of [`WgpuTextureImporter::import`] for
/// [`NativeFrame::DmaBufImage`] frames.
pub(super) fn import_vulkan(
    frame: DmaBufImage,
    ctx: &HostWgpuContext,
) -> Result<ImportedTexture, ImportError> {
    if ctx.backend != InteropBackend::Vulkan {
        return Err(ImportError::BackendMismatch {
            frame: NativeFrameKind::DmaBufImage,
            wgpu: ctx.backend,
        });
    }
    if frame.size.width == 0 || frame.size.height == 0 {
        return Err(ImportError::InvalidFrame("DMABUF image has zero size"));
    }
    if frame.planes().is_empty() {
        return Err(ImportError::InvalidFrame("DMABUF image has no planes"));
    }
    if frame.buffer_count() == 0 {
        return Err(ImportError::InvalidFrame(
            "DMABUF image has no descriptor buffers",
        ));
    }
    validate_image_layout(frame.buffer_count(), frame.planes())?;

    // CEF uses DRM_FORMAT_MOD_INVALID when it reports no explicit modifier.
    // wgpu 30 exposes the capability needed for this deliberate linear
    // fallback. Older rows keep the measured refusal instead of guessing.
    let drm_modifier = if frame.modifier == DRM_FORMAT_MOD_INVALID {
        if !host_can_import_implicit_modifier(&ctx.device) {
            return Err(ImportError::VulkanImport(
                "CEF supplied DRM_FORMAT_MOD_INVALID (implicit modifier). Importing it needs \
                 VK_EXT_image_drm_format_modifier and \
                 Features::VULKAN_EXTERNAL_MEMORY_DMA_BUF. This host device lacks that \
                 capability, and wgpu 28/29 cannot register the resulting foreign image state."
                    .into(),
            ));
        }
        DRM_FORMAT_MOD_LINEAR
    } else {
        frame.modifier
    };

    let frame_size = frame.size;
    let frame_format = frame.format;
    let frame_generation = frame.generation;
    let drm_format = frame.drm_format;
    let graft_host = grafting::HostWgpuContext::new(ctx.device.clone(), ctx.queue.clone());
    let (buffers, planes) = frame.into_owned_parts();
    let planes = graft_planes_from_weld(planes);
    let texture = grafting::vulkan_dmabuf::import_dmabuf(
        grafting::vulkan_dmabuf::VulkanDmaBufImport::new(
            frame_size,
            frame_format,
            drm_format,
            drm_modifier,
            buffers,
            planes,
            grafting::vulkan_dmabuf::VulkanDmaBufQueueOwnership::Foreign,
        )
        .map_err(|error| ImportError::VulkanImport(error.to_string()))?,
        &graft_host,
    )
    .map_err(|error| ImportError::VulkanImport(error.to_string()))?;

    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    Ok(ImportedTexture {
        texture,
        view,
        size: wgpu::Extent3d {
            width: frame_size.width,
            height: frame_size.height,
            depth_or_array_layers: 1,
        },
        format: frame_format,
        generation: frame_generation,
    })
}

fn validate_image_layout(buffer_count: usize, planes: &[DmaBufPlane]) -> Result<(), ImportError> {
    if planes.is_empty() {
        return Err(ImportError::InvalidFrame("DMABUF image has no planes"));
    }
    if buffer_count == 0 {
        return Err(ImportError::InvalidFrame(
            "DMABUF image has no descriptor buffers",
        ));
    }
    if planes
        .iter()
        .any(|plane| plane.buffer_index() >= buffer_count)
    {
        return Err(ImportError::InvalidFrame(
            "DMABUF plane buffer index is out of range",
        ));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FdIdentity {
    device: libc::dev_t,
    inode: libc::ino_t,
}

fn fd_identity(fd: RawFd) -> Option<FdIdentity> {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    if unsafe { libc::fstat(fd, stat.as_mut_ptr()) } != 0 {
        return None;
    }
    let stat = unsafe { stat.assume_init() };
    Some(FdIdentity {
        device: stat.st_dev,
        inode: stat.st_ino,
    })
}

fn deduplicated_raw_planes(
    raw_planes: Vec<(RawFd, u64, u64, u32)>,
) -> Result<(Vec<OwnedFd>, Vec<DmaBufPlane>), ImportError> {
    let mut raw_custody = RawFdCustody::new(&raw_planes);
    let mut raw_fd_indices = Vec::<(RawFd, usize)>::new();
    let mut buffer_identities = Vec::<FdIdentity>::new();
    let mut buffers = Vec::<OwnedFd>::new();
    let mut planes = Vec::with_capacity(raw_planes.len());

    for (raw_fd, offset, size, stride) in raw_planes {
        if raw_fd < 0 {
            return Err(ImportError::InvalidFrame("DMABUF plane fd is negative"));
        }
        let buffer_index = match raw_fd_indices
            .iter()
            .find(|(seen_raw_fd, _)| *seen_raw_fd == raw_fd)
        {
            Some((_, index)) => *index,
            None => {
                let identity = fd_identity(raw_fd)
                    .ok_or(ImportError::InvalidFrame("DMABUF plane fd is invalid"))?;
                match buffer_identities
                    .iter()
                    .position(|existing| *existing == identity)
                {
                    Some(index) => {
                        raw_fd_indices.push((raw_fd, index));
                        raw_custody.disarm(raw_fd);
                        unsafe { libc::close(raw_fd) };
                        index
                    }
                    None => {
                        raw_custody.disarm(raw_fd);
                        let owned = unsafe { OwnedFd::from_raw_fd(raw_fd) };
                        let index = buffers.len();
                        buffers.push(owned);
                        buffer_identities.push(identity);
                        raw_fd_indices.push((raw_fd, index));
                        index
                    }
                }
            }
        };

        planes.push(DmaBufPlane::new(buffer_index, offset, size, stride));
    }

    Ok((buffers, planes))
}

struct RawFdCustody {
    raw_fds: Vec<RawFd>,
}

impl RawFdCustody {
    fn new(raw_planes: &[(RawFd, u64, u64, u32)]) -> Self {
        let mut raw_fds = Vec::new();
        for (raw_fd, ..) in raw_planes {
            if *raw_fd >= 0 && !raw_fds.contains(raw_fd) {
                raw_fds.push(*raw_fd);
            }
        }
        Self { raw_fds }
    }

    fn disarm(&mut self, raw_fd: RawFd) {
        if let Some(position) = self
            .raw_fds
            .iter()
            .position(|candidate| *candidate == raw_fd)
        {
            self.raw_fds.swap_remove(position);
        }
    }
}

impl Drop for RawFdCustody {
    fn drop(&mut self) {
        for raw_fd in &self.raw_fds {
            unsafe {
                libc::close(*raw_fd);
            }
        }
    }
}

fn graft_planes_from_weld(
    planes: Vec<DmaBufPlane>,
) -> Vec<grafting::vulkan_dmabuf::VulkanDmaBufPlane> {
    planes
        .into_iter()
        .map(|plane| grafting::vulkan_dmabuf::VulkanDmaBufPlane {
            buffer_index: plane.buffer_index(),
            offset: plane.offset(),
            stride: u64::from(plane.stride()),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd};

    fn open_pipe_read_fd() -> OwnedFd {
        let mut fds = [0; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        unsafe { libc::close(fds[1]) };
        unsafe { OwnedFd::from_raw_fd(fds[0]) }
    }

    fn fd_is_closed(fd: RawFd) -> bool {
        (unsafe { libc::fcntl(fd, libc::F_GETFD) }) == -1
    }

    #[test]
    fn standalone_plane_metadata_does_not_own_fd() {
        let fd = open_pipe_read_fd();
        let raw = fd.as_raw_fd();
        let plane = DmaBufPlane::new(0, 0, 16, 8);

        drop(plane);

        assert!(
            !fd_is_closed(raw),
            "dropping plane metadata must not close the descriptor"
        );
        drop(fd);
        assert!(fd_is_closed(raw));
    }

    #[test]
    fn invalid_safe_plane_index_closes_owned_buffers() {
        let fd = open_pipe_read_fd();
        let raw = fd.as_raw_fd();

        let result = DmaBufImage::from_owned_buffers(
            vec![fd],
            vec![DmaBufPlane::new(1, 0, 16, 8)],
            PhysicalSize::new(2, 2),
            wgpu::TextureFormat::Bgra8UnormSrgb,
            0,
            DRM_FORMAT_MOD_LINEAR,
            1,
        );

        assert!(result.is_err());
        assert!(fd_is_closed(raw));
    }

    #[test]
    fn repeated_raw_fd_planes_keep_one_owner_until_image_drop() {
        let fd = open_pipe_read_fd();
        let raw = fd.into_raw_fd();

        let image = unsafe {
            DmaBufImage::from_owned_raw_planes(
                vec![(raw, 0, 16, 8), (raw, 4, 12, 8)],
                PhysicalSize::new(2, 2),
                wgpu::TextureFormat::Bgra8UnormSrgb,
                0,
                DRM_FORMAT_MOD_LINEAR,
                1,
            )
        }
        .expect("repeated raw fd should create one image owner");

        assert_eq!(image.buffer_count(), 1);
        assert_eq!(image.planes()[0].buffer_index(), 0);
        assert_eq!(image.planes()[1].buffer_index(), 0);
        assert!(
            !fd_is_closed(raw),
            "repeating a raw fd must not create and drop a second owner"
        );
        drop(image);
        assert!(fd_is_closed(raw));
    }

    #[test]
    fn shared_kernel_object_planes_use_one_graft_buffer() {
        let first = open_pipe_read_fd();
        let second_raw = unsafe { libc::dup(first.as_raw_fd()) };
        assert!(second_raw >= 0);
        let first_raw = first.into_raw_fd();

        let image = unsafe {
            DmaBufImage::from_owned_raw_planes(
                vec![(first_raw, 0, 16, 8), (second_raw, 4, 12, 8)],
                PhysicalSize::new(2, 2),
                wgpu::TextureFormat::Bgra8UnormSrgb,
                0,
                DRM_FORMAT_MOD_LINEAR,
                1,
            )
        }
        .expect("dup fds for one kernel object should create one image owner");

        assert_eq!(image.buffer_count(), 1);
        assert_eq!(image.planes()[0].buffer_index(), 0);
        assert_eq!(image.planes()[1].buffer_index(), 0);
        assert_eq!(image.planes()[1].offset(), 4);
        assert_eq!(image.planes()[1].stride(), 8);
        assert!(
            fd_is_closed(second_raw),
            "the duplicate plane fd should close before Graft handoff"
        );
        assert!(!fd_is_closed(first_raw));
        drop(image);
        assert!(fd_is_closed(first_raw));
    }

    #[test]
    fn raw_constructor_failure_closes_accepted_buffers() {
        let first = open_pipe_read_fd();
        let first_raw = first.into_raw_fd();

        let result = unsafe {
            DmaBufImage::from_owned_raw_planes(
                vec![(first_raw, 0, 16, 8), (-1, 0, 16, 8)],
                PhysicalSize::new(2, 2),
                wgpu::TextureFormat::Bgra8UnormSrgb,
                0,
                DRM_FORMAT_MOD_LINEAR,
                1,
            )
        };

        assert!(result.is_err());
        assert!(fd_is_closed(first_raw));
    }

    #[test]
    fn raw_constructor_error_closes_later_unprocessed_fds() {
        let first = open_pipe_read_fd();
        let second = open_pipe_read_fd();
        let first_raw = first.into_raw_fd();
        let second_raw = second.into_raw_fd();

        let result = unsafe {
            DmaBufImage::from_owned_raw_planes(
                vec![
                    (first_raw, 0, 16, 8),
                    (-1, 0, 16, 8),
                    (second_raw, 4, 12, 8),
                ],
                PhysicalSize::new(2, 2),
                wgpu::TextureFormat::Bgra8UnormSrgb,
                0,
                DRM_FORMAT_MOD_LINEAR,
                1,
            )
        };

        assert!(result.is_err());
        assert!(fd_is_closed(first_raw));
        assert!(
            fd_is_closed(second_raw),
            "error cleanup must close valid raw fds that appear after the failing entry"
        );
    }

    #[test]
    fn graft_constructor_error_closes_image_owned_buffers() {
        let first = open_pipe_read_fd();
        let second = open_pipe_read_fd();
        let first_raw = first.as_raw_fd();
        let second_raw = second.as_raw_fd();
        let image = DmaBufImage::from_owned_buffers(
            vec![first, second],
            vec![DmaBufPlane::new(0, 0, 16, 8), DmaBufPlane::new(1, 0, 16, 8)],
            PhysicalSize::new(2, 2),
            wgpu::TextureFormat::Bgra8UnormSrgb,
            0,
            DRM_FORMAT_MOD_LINEAR,
            1,
        )
        .expect("indices are in range");
        let (buffers, planes) = image.into_owned_parts();
        let graft_planes = graft_planes_from_weld(planes);
        assert_eq!(buffers.len(), 2);

        let result = grafting::vulkan_dmabuf::VulkanDmaBufImport::new(
            PhysicalSize::new(2, 2),
            wgpu::TextureFormat::Bgra8UnormSrgb,
            0,
            DRM_FORMAT_MOD_LINEAR,
            buffers,
            graft_planes,
            grafting::vulkan_dmabuf::VulkanDmaBufQueueOwnership::Foreign,
        );

        assert!(result.is_err());
        assert!(fd_is_closed(first_raw));
        assert!(fd_is_closed(second_raw));
    }
}
