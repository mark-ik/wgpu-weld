//! Linux: CEF DMABUF policy -> Graft -> Vulkan/wgpu.
//!
//! Welding decides how to handle CEF's implicit modifier. Graft owns explicit
//! DRM-modifier image creation, memory import, fd lifetime, compatible memory
//! selection, shared-fd planes, and foreign-queue acquisition.

use super::*;

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
    let graft_host = grafting::HostWgpuContext::new(ctx.device.clone(), ctx.queue.clone());
    let planes = frame
        .forget_fds()
        .into_iter()
        .map(|plane| grafting::vulkan_dmabuf::VulkanDmaBufPlane {
            fd: plane.fd,
            offset: plane.offset,
            stride: u64::from(plane.stride),
        })
        .collect();
    let texture = grafting::vulkan_dmabuf::import_dmabuf(
        grafting::vulkan_dmabuf::VulkanDmaBufImport {
            size: frame_size,
            format: frame_format,
            drm_format,
            drm_modifier,
            planes,
            queue_ownership: grafting::vulkan_dmabuf::VulkanDmaBufQueueOwnership::Foreign,
        },
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
