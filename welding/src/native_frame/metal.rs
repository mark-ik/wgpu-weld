//! macOS: retained `IOSurfaceRef` -> `MTLTexture` -> Graft -> wgpu Metal.
//!
//! Welding owns the CEF/IOSurface lifetime and texture descriptor. Graft owns
//! the backend-specific `MTLTexture` -> `wgpu::Texture` wrapper.

use super::*;
use std::ffi::c_void;

/// The Apple half of [`WgpuTextureImporter::import`] for
/// [`NativeFrame::MetalTextureRef`] frames.
pub(super) fn import_metal(
    frame: MetalTextureRef,
    ctx: &HostWgpuContext,
) -> Result<ImportedTexture, ImportError> {
    use objc2::rc::Retained;
    use objc2_io_surface::IOSurfaceRef;
    use objc2_metal::{MTLDevice, MTLStorageMode, MTLTextureDescriptor, MTLTextureUsage};

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

    let mtl_texture = unsafe {
        let mtl_device =
            crate::wgpu_compat::metal_device(&ctx.device).ok_or(ImportError::BackendMismatch {
                frame: NativeFrameKind::MetalTextureRef,
                wgpu: ctx.backend,
            })?;

        // `newTextureWithDescriptor:iosurface:plane:` takes the CoreFoundation
        // `IOSurfaceRef`, not the ObjC `IOSurface` class, and CEF hands us the
        // retained CF pointer.
        let io_surface = &*(frame.io_surface as *const IOSurfaceRef);
        let descriptor =
            MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
                pixel_format,
                frame.size.width as usize,
                frame.size.height as usize,
                false,
            );
        descriptor.setStorageMode(MTLStorageMode::Shared);
        descriptor.setUsage(MTLTextureUsage::ShaderRead);

        mtl_device
            .newTextureWithDescriptor_iosurface_plane(&descriptor, io_surface, 0)
            .ok_or_else(|| {
                ImportError::MetalImport(
                    "MTLDevice::newTextureWithDescriptor:iosurface:plane: returned nil".into(),
                )
            })?
    };

    let graft_host = grafting::HostWgpuContext::new(ctx.device.clone(), ctx.queue.clone());
    let graft_frame = grafting::MetalTextureRef {
        size: frame.size,
        format: frame.format,
        generation: frame.generation,
        producer_sync: grafting::SyncMechanism::None,
        raw_metal_texture: Retained::as_ptr(&mtl_texture) as *mut c_void,
    };
    let texture = grafting::import_metal_texture_ref(&graft_frame, &graft_host)
        .map_err(|error| ImportError::MetalImport(error.to_string()))?;

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
