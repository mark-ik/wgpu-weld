//! Linux: DMABUF planes -> Vulkan external memory -> wgpu Vulkan.
//!
//! Split out of `native_frame/mod.rs`. Ported from
//! `wgpu-graft/grafting/src/vulkan_dmabuf.rs`. For the Phase 4 first cut we
//! accept only single-plane formats (BGRA8 / RGBA8 from CEF); multi-plane
//! (e.g. NV12 video) is deferred until CEF actually emits it on this path.

use super::*;

/// `DRM_FORMAT_MOD_INVALID` from `drm_fourcc.h`: "no explicit modifier".
const DRM_FORMAT_MOD_INVALID: u64 = 0x00ff_ffff_ffff_ffff;

/// `DRM_FORMAT_MOD_LINEAR` from `drm_fourcc.h`: no tiling, row-major.
const DRM_FORMAT_MOD_LINEAR: u64 = 0;

/// Can the host's device import a buffer whose modifier CEF left implicit?
///
/// The capability is `VK_EXT_image_drm_format_modifier`, which wgpu 30 enables
/// on the device whenever the adapter supports it and surfaces as
/// [`wgpu::Features::VULKAN_EXTERNAL_MEMORY_DMA_BUF`]. The feature is the
/// signal to gate on: it is absent from wgpu 28 and 29 entirely, so those rows
/// answer `false` and keep the old refusal.
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
    use ash::vk;

    if ctx.backend != InteropBackend::Vulkan {
        return Err(ImportError::BackendMismatch {
            frame: NativeFrameKind::DmaBufImage,
            wgpu: ctx.backend,
        });
    }
    if frame.size.width == 0 || frame.size.height == 0 {
        return Err(ImportError::InvalidFrame("DMABUF image has zero size"));
    }
    if frame.planes.len() != 1 {
        return Err(ImportError::InvalidFrame(
            "multi-plane DMABUF formats not supported in Phase 4",
        ));
    }
    let plane = frame.planes[0];
    if plane.fd < 0 {
        return Err(ImportError::InvalidFrame("DMABUF plane fd is negative"));
    }
    // CEF hands over DRM_FORMAT_MOD_INVALID when the buffer carries no
    // explicit modifier. AMD/RADV does this in practice; Intel/Mesa supplies a
    // real modifier, which is why the original Phase 4 verification passed.
    //
    // On wgpu 28 and 29 such a buffer could not be imported at all, and that
    // was established on real hardware rather than reasoned about. The
    // DRM_FORMAT_MODIFIER_EXT tiling this function uses needs
    // VK_EXT_image_drm_format_modifier on the device, neither row enabled it,
    // and vkCreateImage answered VK_ERROR_FORMAT_NOT_SUPPORTED, with the
    // validation layer sometimes aborting the process while formatting its own
    // error about it. Plain LINEAR tiling fared no better: the format query
    // rejects DMA_BUF as a handle type for it (VUID-VkImageCreateInfo-pNext-
    // 00990) and the resulting texture panicked inside wgpu on first use.
    //
    // wgpu 30 changed the picture. It enables the extension whenever the
    // adapter supports it, and exposes VULKAN_EXTERNAL_MEMORY_DMA_BUF as the
    // capability signal. When the host device carries that feature, import the
    // implicit-modifier buffer as DRM_FORMAT_MOD_LINEAR.
    //
    // That substitution is an assumption rather than something CEF told us,
    // and on AMD an implicit layout may well be tiled. So a successful import
    // is not proof of a correct one: the pixels have to be checked, which is
    // what demo-weld-linux's readback probe is for.
    let drm_modifier = if frame.modifier == DRM_FORMAT_MOD_INVALID {
        if !host_can_import_implicit_modifier(&ctx.device) {
            return Err(ImportError::VulkanImport(
                "CEF supplied DRM_FORMAT_MOD_INVALID (implicit modifier). Importing it needs \
                 VK_EXT_image_drm_format_modifier, which arrived in wgpu 30 together with \
                 Features::VULKAN_EXTERNAL_MEMORY_DMA_BUF. This host device does not carry that \
                 feature, and wgpu 28/29 cannot provide it at all. Seen on AMD/RADV; Intel/Mesa \
                 supplies an explicit modifier and needs none of this."
                    .into(),
            ));
        }
        DRM_FORMAT_MOD_LINEAR
    } else {
        frame.modifier
    };

    let vk_format = match frame.format {
        wgpu::TextureFormat::Rgba8UnormSrgb => vk::Format::R8G8B8A8_SRGB,
        wgpu::TextureFormat::Bgra8UnormSrgb => vk::Format::B8G8R8A8_SRGB,
        wgpu::TextureFormat::Rgba8Unorm => vk::Format::R8G8B8A8_UNORM,
        wgpu::TextureFormat::Bgra8Unorm => vk::Format::B8G8R8A8_UNORM,
        other => {
            return Err(ImportError::VulkanImport(format!(
                "unsupported texture format for DMABUF import: {other:?}"
            )));
        }
    };

    let extent = vk::Extent3D {
        width: frame.size.width,
        height: frame.size.height,
        depth: 1,
    };
    let frame_size = frame.size;
    let frame_format = frame.format;
    let frame_generation = frame.generation;

    unsafe {
        let hal_device = ctx
            .device
            .as_hal::<wgpu::wgc::api::Vulkan>()
            .ok_or(ImportError::BackendMismatch {
                frame: NativeFrameKind::DmaBufImage,
                wgpu: ctx.backend,
            })?;
        let vk_device = hal_device.raw_device().clone();
        let vk_instance = hal_device.shared_instance().raw_instance().clone();
        let physical_device = hal_device.raw_physical_device();

        let plane_layouts = [vk::SubresourceLayout {
            offset: plane.offset,
            size: 0,
            row_pitch: plane.stride as u64,
            array_pitch: 0,
            depth_pitch: 0,
        }];
        let mut drm_modifier_info = vk::ImageDrmFormatModifierExplicitCreateInfoEXT::default()
            .drm_format_modifier(drm_modifier)
            .plane_layouts(&plane_layouts);
        let mut external_memory_info = vk::ExternalMemoryImageCreateInfo::default()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);

        let image_create_info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(vk_format)
            .extent(extent)
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
            // TRANSFER_SRC as well as SAMPLED: hosts read the imported texture
            // back to check it, which the macOS path has always allowed and
            // this one did not. Without it wgpu rejects the copy for missing
            // COPY_SRC, and on Linux that rejection was invisible behind the
            // implicit-modifier refusal above.
            .usage(vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_SRC)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .push_next(&mut external_memory_info)
            .push_next(&mut drm_modifier_info);

        let vulkan_image = vk_device
            .create_image(&image_create_info, None)
            .map_err(|err| ImportError::VulkanImport(format!("create_image (dmabuf): {err}")))?;

        // Vulkan takes ownership of the fd on a successful allocate_memory. We
        // forget the DmaBufImage's fds upfront so its Drop won't double-close
        // on error; on the error path we close the fd ourselves below.
        let planes = frame.forget_fds();
        let fd = planes[0].fd;

        let memory = match allocate_and_bind_dmabuf_memory(
            &vk_device,
            &vk_instance,
            physical_device,
            vulkan_image,
            fd,
        ) {
            Ok(memory) => memory,
            Err(err) => {
                // allocate_memory failed before taking ownership; close manually.
                libc::close(fd);
                vk_device.destroy_image(vulkan_image, None);
                return Err(err);
            }
        };

        let vk_device_for_drop = vk_device.clone();
        let imported = crate::wgpu_compat::create_texture_from_hal::<wgpu::wgc::api::Vulkan>(
            &ctx.device,
            hal_device.texture_from_raw(
                vulkan_image,
                &wgpu_hal::TextureDescriptor {
                    label: Some("welding-cef-dmabuf-vulkan-import"),
                    size: wgpu::Extent3d {
                        width: frame_size.width,
                        height: frame_size.height,
                        depth_or_array_layers: 1,
                    },
                    format: frame_format,
                    dimension: wgpu::TextureDimension::D2,
                    mip_level_count: 1,
                    sample_count: 1,
                    usage: wgpu::TextureUses::RESOURCE | wgpu::TextureUses::COPY_SRC,
                    view_formats: Vec::new(),
                    memory_flags: wgpu_hal::MemoryFlags::empty(),
                },
                Some(Box::new(move || {
                    vk_device_for_drop.destroy_image(vulkan_image, None);
                    vk_device_for_drop.free_memory(memory, None);
                })),
                wgpu_hal::vulkan::TextureMemory::External,
            ),
            &wgpu::TextureDescriptor {
                label: Some("welding-cef-dmabuf-vulkan-import"),
                size: wgpu::Extent3d {
                    width: frame_size.width,
                    height: frame_size.height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: frame_format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            },
            // The locally created image has not left UNDEFINED layout.
            wgpu::TextureUses::UNINITIALIZED,
        );

        let view = imported.create_view(&wgpu::TextureViewDescriptor::default());
        Ok(ImportedTexture {
            texture: imported,
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
}

unsafe fn allocate_and_bind_dmabuf_memory(
    vk_device: &ash::Device,
    vk_instance: &ash::Instance,
    physical_device: ash::vk::PhysicalDevice,
    vulkan_image: ash::vk::Image,
    dmabuf_fd: i32,
) -> Result<ash::vk::DeviceMemory, ImportError> {
    use ash::vk;

    let external_memory_fd_api =
        ash::khr::external_memory_fd::Device::new(vk_instance, vk_device);

    let mut fd_properties = vk::MemoryFdPropertiesKHR::default();
    unsafe {
        external_memory_fd_api
            .get_memory_fd_properties(
                vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT,
                dmabuf_fd,
                &mut fd_properties,
            )
            .map_err(|err| {
                ImportError::VulkanImport(format!("get_memory_fd_properties: {err}"))
            })?;
    }

    let memory_requirements = unsafe { vk_device.get_image_memory_requirements(vulkan_image) };
    let allowed_memory_type_bits =
        memory_requirements.memory_type_bits & fd_properties.memory_type_bits;
    let memory_properties =
        unsafe { vk_instance.get_physical_device_memory_properties(physical_device) };
    let memory_type_index = memory_properties.memory_types
        [..memory_properties.memory_type_count as usize]
        .iter()
        .enumerate()
        .position(|(i, _)| (allowed_memory_type_bits & (1 << i)) != 0)
        .ok_or_else(|| {
            ImportError::VulkanImport("no memory type compatible with dmabuf import".into())
        })? as u32;

    let mut import_memory_info = vk::ImportMemoryFdInfoKHR::default()
        .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
        .fd(dmabuf_fd);
    let mut dedicated_allocate_info =
        vk::MemoryDedicatedAllocateInfo::default().image(vulkan_image);

    let memory = unsafe {
        vk_device
            .allocate_memory(
                &vk::MemoryAllocateInfo::default()
                    .allocation_size(memory_requirements.size)
                    .memory_type_index(memory_type_index)
                    .push_next(&mut import_memory_info)
                    .push_next(&mut dedicated_allocate_info),
                None,
            )
            .map_err(|err| {
                ImportError::VulkanImport(format!("allocate_memory (dmabuf import): {err}"))
            })?
    };

    if let Err(err) = unsafe { vk_device.bind_image_memory(vulkan_image, memory, 0) } {
        unsafe { vk_device.free_memory(memory, None) };
        return Err(ImportError::VulkanImport(format!(
            "bind_image_memory: {err}"
        )));
    }

    Ok(memory)
}
