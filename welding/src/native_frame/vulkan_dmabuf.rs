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
use std::os::fd::{FromRawFd, OwnedFd, RawFd};

/// `DRM_FORMAT_MOD_INVALID` from `drm_fourcc.h`: no explicit modifier.
const DRM_FORMAT_MOD_INVALID: u64 = 0x00ff_ffff_ffff_ffff;

/// `DRM_FORMAT_MOD_LINEAR` from `drm_fourcc.h`: no tiling, row-major.
const DRM_FORMAT_MOD_LINEAR: u64 = 0;

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
    if frame.planes.is_empty() {
        return Err(ImportError::InvalidFrame("DMABUF image has no planes"));
    }
    if frame.planes.iter().any(|plane| plane.fd < 0) {
        return Err(ImportError::InvalidFrame("DMABUF plane fd is negative"));
    }

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
    let plane_identities = plane_fd_identities(&frame.planes)?;
    let graft_host = grafting::HostWgpuContext::new(ctx.device.clone(), ctx.queue.clone());
    let (buffers, planes) = deduplicated_owned_planes(frame.forget_fds(), plane_identities);
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FdIdentity {
    device: libc::dev_t,
    inode: libc::ino_t,
}

fn plane_fd_identities(planes: &[DmaBufPlane]) -> Result<Vec<FdIdentity>, ImportError> {
    planes
        .iter()
        .map(|plane| {
            fd_identity(plane.fd).ok_or(ImportError::InvalidFrame("DMABUF plane fd is invalid"))
        })
        .collect()
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

fn deduplicated_owned_planes(
    planes: Vec<DmaBufPlane>,
    identities: Vec<FdIdentity>,
) -> (
    Vec<OwnedFd>,
    Vec<grafting::vulkan_dmabuf::VulkanDmaBufPlane>,
) {
    debug_assert_eq!(planes.len(), identities.len());

    let mut raw_fd_indices = Vec::<(RawFd, usize)>::new();
    let mut buffer_identities = Vec::<FdIdentity>::new();
    let mut buffers = Vec::<OwnedFd>::new();
    let mut graft_planes = Vec::with_capacity(planes.len());

    for (plane, identity) in planes.into_iter().zip(identities) {
        let buffer_index = match raw_fd_indices
            .iter()
            .find(|(raw_fd, _)| *raw_fd == plane.fd)
        {
            Some((_, index)) => *index,
            None => {
                let owned = unsafe { OwnedFd::from_raw_fd(plane.fd) };
                let index = match buffer_identities
                    .iter()
                    .position(|existing| *existing == identity)
                {
                    Some(index) => {
                        drop(owned);
                        index
                    }
                    None => {
                        let index = buffers.len();
                        buffers.push(owned);
                        buffer_identities.push(identity);
                        index
                    }
                };
                raw_fd_indices.push((plane.fd, index));
                index
            }
        };

        graft_planes.push(grafting::vulkan_dmabuf::VulkanDmaBufPlane {
            buffer_index,
            offset: plane.offset,
            stride: u64::from(plane.stride),
        });
    }

    (buffers, graft_planes)
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
    fn repeated_raw_fd_planes_keep_one_owner_until_buffer_drop() {
        let fd = open_pipe_read_fd();
        let raw = fd.into_raw_fd();
        let planes = vec![
            unsafe { DmaBufPlane::from_owned_raw_fd(raw, 0, 16, 8) },
            unsafe { DmaBufPlane::from_owned_raw_fd(raw, 4, 12, 8) },
        ];
        let identities = plane_fd_identities(&planes).expect("pipe fd should be valid");

        let (buffers, graft_planes) = deduplicated_owned_planes(planes, identities);

        assert_eq!(buffers.len(), 1);
        assert_eq!(graft_planes[0].buffer_index, 0);
        assert_eq!(graft_planes[1].buffer_index, 0);
        assert!(
            !fd_is_closed(raw),
            "repeating a raw fd must not create and drop a second owner"
        );
        drop(buffers);
        assert!(fd_is_closed(raw));
    }

    #[test]
    fn shared_kernel_object_planes_use_one_graft_buffer() {
        let first = open_pipe_read_fd();
        let second_raw = unsafe { libc::dup(first.as_raw_fd()) };
        assert!(second_raw >= 0);
        let first_raw = first.into_raw_fd();
        let planes = vec![
            unsafe { DmaBufPlane::from_owned_raw_fd(first_raw, 0, 16, 8) },
            unsafe { DmaBufPlane::from_owned_raw_fd(second_raw, 4, 12, 8) },
        ];
        let identities = plane_fd_identities(&planes).expect("pipe fds should be valid");

        let (buffers, graft_planes) = deduplicated_owned_planes(planes, identities);

        assert_eq!(buffers.len(), 1);
        assert_eq!(graft_planes[0].buffer_index, 0);
        assert_eq!(graft_planes[1].buffer_index, 0);
        assert_eq!(graft_planes[1].offset, 4);
        assert_eq!(graft_planes[1].stride, 8);
        assert!(
            fd_is_closed(second_raw),
            "the duplicate plane fd should close before Graft handoff"
        );
        drop(buffers);
        assert!(fd_is_closed(first_raw));
    }

    #[test]
    fn graft_constructor_error_closes_weld_owned_buffers() {
        let first = open_pipe_read_fd();
        let second = open_pipe_read_fd();
        let first_raw = first.into_raw_fd();
        let second_raw = second.into_raw_fd();
        let planes = vec![
            unsafe { DmaBufPlane::from_owned_raw_fd(first_raw, 0, 16, 8) },
            unsafe { DmaBufPlane::from_owned_raw_fd(second_raw, 0, 16, 8) },
        ];
        let identities = plane_fd_identities(&planes).expect("pipe fds should be valid");
        let (buffers, graft_planes) = deduplicated_owned_planes(planes, identities);
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
