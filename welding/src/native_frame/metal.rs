//! macOS: retained `IOSurfaceRef` -> `MTLTexture` -> wgpu Metal.
//!
//! Split out of `native_frame/mod.rs`. Import code is written but still
//! pending runtime validation on a real macOS host.

use super::*;

/// The Apple half of [`WgpuTextureImporter::import`] for
/// [`NativeFrame::MetalTextureRef`] frames.
pub(super) fn import_metal(
    frame: MetalTextureRef,
    ctx: &HostWgpuContext,
) -> Result<ImportedTexture, ImportError> {
    use objc2_io_surface::IOSurfaceRef;
    use objc2_metal::{
        MTLDevice, MTLStorageMode, MTLTextureDescriptor, MTLTextureType, MTLTextureUsage,
    };
    use std::ffi::c_void;

    if frame.io_surface.is_null() {
        return Err(ImportError::InvalidFrame("IOSurface handle is null"));
    }
    if frame.size.width == 0 || frame.size.height == 0 {
        return Err(ImportError::InvalidFrame("Metal IOSurface has zero size"));
    }
    if ctx.backend != InteropBackend::Metal {
        return Err(ImportError::BackendMismatch {
            frame: NativeFrameKind::MetalTextureRef,
            wgpu: ctx.backend,
        });
    }

    let pixel_format = wgpu_format_to_mtl(frame.format).ok_or_else(|| {
        ImportError::MetalImport(format!(
            "unsupported wgpu format for Metal IOSurface import: {:?}",
            frame.format
        ))
    })?;

    unsafe extern "C" {
        fn CFRelease(cf: *const c_void);
    }

    let texture = unsafe {
        let hal_device = ctx
            .device
            .as_hal::<wgpu::wgc::api::Metal>()
            .ok_or_else(|| ImportError::BackendMismatch {
                frame: NativeFrameKind::MetalTextureRef,
                wgpu: ctx.backend,
            })?;
        let mtl_device = hal_device.raw_device();

        // `newTextureWithDescriptor:iosurface:plane:` takes the CoreFoundation
        // `IOSurfaceRef`, not the ObjC `IOSurface` class, and CEF hands us the
        // CF pointer.
        let io_surf = &*(frame.io_surface as *const IOSurfaceRef);
        let desc = MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
            pixel_format,
            frame.size.width as usize,
            frame.size.height as usize,
            false,
        );
        desc.setStorageMode(MTLStorageMode::Shared);
        desc.setUsage(MTLTextureUsage::ShaderRead);

        let create_result =
            mtl_device.newTextureWithDescriptor_iosurface_plane(&desc, io_surf, 0);
        // Release our retain; Metal holds its own reference after create.
        CFRelease(frame.io_surface);
        let mtl_texture = create_result.ok_or_else(|| {
            ImportError::MetalImport(
                "MTLDevice::newTextureWithDescriptor:iosurface:plane: returned nil".into(),
            )
        })?;

        let hal_texture = wgpu_hal::metal::Device::texture_from_raw(
            mtl_texture,
            frame.format,
            MTLTextureType::Type2D,
            1,
            1,
            wgpu_hal::CopyExtent {
                width: frame.size.width,
                height: frame.size.height,
                depth: 1,
            },
        );

        ctx.device.create_texture_from_hal::<wgpu::wgc::api::Metal>(
            hal_texture,
            &wgpu::TextureDescriptor {
                label: Some("weld-cef-metal-iosurface-import"),
                size: wgpu::Extent3d {
                    width: frame.size.width,
                    height: frame.size.height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: frame.format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            },
        )
    };

    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    Ok(ImportedTexture {
        texture,
        view,
        size: wgpu::Extent3d {
            width: frame.size.width,
            height: frame.size.height,
            depth_or_array_layers: 1,
        },
        format: frame.format,
        generation: frame.generation,
    })
}

fn wgpu_format_to_mtl(format: wgpu::TextureFormat) -> Option<objc2_metal::MTLPixelFormat> {
    use objc2_metal::MTLPixelFormat;
    match format {
        wgpu::TextureFormat::Bgra8Unorm => Some(MTLPixelFormat::BGRA8Unorm),
        wgpu::TextureFormat::Rgba8Unorm => Some(MTLPixelFormat::RGBA8Unorm),
        _ => None,
    }
}
