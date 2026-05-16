//! Native GPU surface handles produced by CEF's `OnAcceleratedPaint` callback,
//! plus the wgpu import infrastructure.
//!
//! This is the CEF-shaped analogue of `wgpu-scry::native_frame` and
//! `wgpu-graft::grafting`: the producer gives us a borrowed native resource in
//! the paint callback, `weld` duplicates or retains that resource immediately,
//! and the host later imports the owned handle into its `wgpu::Device`.

use dpi::PhysicalSize;

// ── Backend detection ─────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum InteropBackend {
    Vulkan,
    Metal,
    Dx12,
    Unknown,
}

impl InteropBackend {
    pub fn detect(device: &wgpu::Device) -> Self {
        unsafe {
            #[cfg(any(target_os = "linux", target_os = "android", target_os = "windows"))]
            if device.as_hal::<wgpu::wgc::api::Vulkan>().is_some() {
                return InteropBackend::Vulkan;
            }

            #[cfg(target_vendor = "apple")]
            if device.as_hal::<wgpu::wgc::api::Metal>().is_some() {
                return InteropBackend::Metal;
            }

            #[cfg(target_os = "windows")]
            if device.as_hal::<wgpu::wgc::api::Dx12>().is_some() {
                return InteropBackend::Dx12;
            }
        }

        InteropBackend::Unknown
    }
}

// ── Native frame types ────────────────────────────────────────────────────────

/// A GPU surface handle emitted by CEF's `OnAcceleratedPaint` callback,
/// ready to be imported into the host's wgpu pipeline.
#[derive(Debug)]
#[non_exhaustive]
pub enum NativeFrame {
    /// Windows: shared D3D11 texture handle from `CefAcceleratedPaintInfo`.
    /// Imported via the D3D11 open-shared → D3D12 resource path.
    ///
    /// # Handle lifetime
    /// The underlying `HANDLE` is valid only for the duration of
    /// `OnAcceleratedPaint`. `WindowsCefProducer` either imports it
    /// synchronously within the callback or `DuplicateHandle`s it first.
    Dx12SharedTexture(Dx12SharedTexture),
    /// macOS: `IOSurfaceRef` from `CefAcceleratedPaintInfo`.
    /// Imported as a Metal-backed wgpu texture.
    MetalTextureRef(MetalTextureRef),
    /// Linux: native pixmap / DMABUF planes from accelerated OSR.
    DmaBufImage(DmaBufImage),
    /// CPU bitmap from `OnPaint` (requires `feature = "cpu-paint-fallback"`).
    #[cfg(feature = "cpu-paint-fallback")]
    CpuBitmap(CpuBitmap),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum NativeFrameKind {
    Dx12SharedTexture,
    MetalTextureRef,
    DmaBufImage,
    #[cfg(feature = "cpu-paint-fallback")]
    CpuBitmap,
}

impl NativeFrame {
    pub fn kind(&self) -> NativeFrameKind {
        match self {
            NativeFrame::Dx12SharedTexture(_) => NativeFrameKind::Dx12SharedTexture,
            NativeFrame::MetalTextureRef(_) => NativeFrameKind::MetalTextureRef,
            NativeFrame::DmaBufImage(_) => NativeFrameKind::DmaBufImage,
            #[cfg(feature = "cpu-paint-fallback")]
            NativeFrame::CpuBitmap(_) => NativeFrameKind::CpuBitmap,
        }
    }

    pub fn size(&self) -> PhysicalSize<u32> {
        match self {
            NativeFrame::Dx12SharedTexture(frame) => frame.size,
            NativeFrame::MetalTextureRef(frame) => frame.size,
            NativeFrame::DmaBufImage(frame) => frame.size,
            #[cfg(feature = "cpu-paint-fallback")]
            NativeFrame::CpuBitmap(frame) => frame.size,
        }
    }

    pub fn format(&self) -> wgpu::TextureFormat {
        match self {
            NativeFrame::Dx12SharedTexture(frame) => frame.format,
            NativeFrame::MetalTextureRef(frame) => frame.format,
            NativeFrame::DmaBufImage(frame) => frame.format,
            #[cfg(feature = "cpu-paint-fallback")]
            NativeFrame::CpuBitmap(frame) => frame.format,
        }
    }

    pub fn generation(&self) -> u64 {
        match self {
            NativeFrame::Dx12SharedTexture(frame) => frame.generation,
            NativeFrame::MetalTextureRef(frame) => frame.generation,
            NativeFrame::DmaBufImage(frame) => frame.generation,
            #[cfg(feature = "cpu-paint-fallback")]
            NativeFrame::CpuBitmap(frame) => frame.generation,
        }
    }
}

// Windows
#[derive(Clone, Copy, Debug)]
pub struct Dx12SharedTexture {
    /// Owned duplicated Win32 `HANDLE` to CEF's shared D3D texture.
    ///
    /// CEF's callback-scoped handle is not stored directly. The Windows
    /// producer must call `DuplicateHandle` before building this frame, and the
    /// importer closes this owned duplicate after opening its D3D12 resource.
    pub handle: *mut std::os::raw::c_void,
    pub size: PhysicalSize<u32>,
    pub format: wgpu::TextureFormat,
    pub generation: u64,
}

unsafe impl Send for Dx12SharedTexture {}

// macOS
#[derive(Clone, Copy, Debug)]
pub struct MetalTextureRef {
    /// Retained `IOSurfaceRef`. Released after wgpu import once the Metal path
    /// is wired.
    pub io_surface: *mut std::os::raw::c_void,
    pub size: PhysicalSize<u32>,
    pub format: wgpu::TextureFormat,
    pub generation: u64,
}

unsafe impl Send for MetalTextureRef {}

#[derive(Clone, Copy, Debug)]
pub struct DmaBufPlane {
    /// Owned DMABUF file descriptor. `weld` duplicates callback-scoped FDs
    /// before storing them here; the Vulkan importer closes them after import.
    pub fd: i32,
    pub offset: u32,
    pub stride: u32,
}

// Linux
#[derive(Clone, Debug)]
pub struct DmaBufImage {
    pub planes: Vec<DmaBufPlane>,
    pub size: PhysicalSize<u32>,
    pub format: wgpu::TextureFormat,
    pub drm_format: u32,
    pub modifier: u64,
    pub generation: u64,
}

#[cfg(feature = "cpu-paint-fallback")]
#[derive(Clone, Debug)]
pub struct CpuBitmap {
    /// BGRA8 pixel data (matches CEF's OnPaint buffer format).
    pub data: Vec<u8>,
    pub size: PhysicalSize<u32>,
    pub format: wgpu::TextureFormat,
    pub generation: u64,
}

/// Single-slot latest-frame mailbox used by the CEF render callback and host
/// thread. New paints replace old paints; browser hosts normally only want the
/// newest frame available at render time.
#[derive(Debug, Default)]
pub struct PendingFrameSlot {
    frame: Option<NativeFrame>,
    next_generation: u64,
}

impl PendingFrameSlot {
    pub fn next_generation(&mut self) -> u64 {
        self.next_generation += 1;
        self.next_generation
    }

    pub fn store(&mut self, frame: NativeFrame) {
        self.frame = Some(frame);
    }

    pub fn take(&mut self) -> Option<NativeFrame> {
        self.frame.take()
    }

    pub fn has_frame(&self) -> bool {
        self.frame.is_some()
    }
}

// ── Host context ──────────────────────────────────────────────────────────────

/// Wraps the host's wgpu device and queue alongside the detected interop backend.
/// Passed to [`WgpuTextureImporter::import`] and [`CefSurfaceProducer::acquire_frame`].
pub struct HostWgpuContext {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub backend: InteropBackend,
}

impl HostWgpuContext {
    pub fn new(device: wgpu::Device, queue: wgpu::Queue) -> Self {
        let backend = InteropBackend::detect(&device);
        HostWgpuContext {
            device,
            queue,
            backend,
        }
    }
}

// ── Import result ─────────────────────────────────────────────────────────────

pub struct ImportedTexture {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub size: wgpu::Extent3d,
    pub format: wgpu::TextureFormat,
    pub generation: u64,
}

// ── Importer ─────────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ImportError {
    #[error("backend mismatch: frame is {frame:?} but wgpu reports {wgpu:?}")]
    BackendMismatch {
        frame: NativeFrameKind,
        wgpu: InteropBackend,
    },
    #[error("invalid native frame: {0}")]
    InvalidFrame(&'static str),
    #[error("native import is not implemented for {0:?}")]
    Unsupported(NativeFrameKind),
    #[error("D3D11 open-shared-resource failed: {0}")]
    D3d11OpenShared(String),
    #[error("D3D12 open-shared-handle failed: {0}")]
    D3d12OpenShared(String),
    #[error("Metal IOSurface import failed: {0}")]
    MetalImport(String),
    #[error("Vulkan external memory import failed: {0}")]
    VulkanImport(String),
    #[error("wgpu-hal error: {0}")]
    Hal(String),
}

/// Converts a [`NativeFrame`] into an [`ImportedTexture`] bound to the host's
/// wgpu device. The import path is chosen by the frame kind and the detected
/// backend in [`HostWgpuContext`].
pub struct WgpuTextureImporter;

impl WgpuTextureImporter {
    pub fn import(
        frame: NativeFrame,
        ctx: &HostWgpuContext,
    ) -> Result<ImportedTexture, ImportError> {
        match frame {
            NativeFrame::Dx12SharedTexture(f) => Self::import_dx12(f, ctx),
            NativeFrame::MetalTextureRef(f) => Self::import_metal(f, ctx),
            NativeFrame::DmaBufImage(f) => Self::import_vulkan(f, ctx),
            #[cfg(feature = "cpu-paint-fallback")]
            NativeFrame::CpuBitmap(f) => Self::upload_cpu(f, ctx),
        }
    }

    fn import_dx12(
        frame: Dx12SharedTexture,
        ctx: &HostWgpuContext,
    ) -> Result<ImportedTexture, ImportError> {
        #[cfg(windows)]
        {
            use windows::Win32::Foundation::{CloseHandle, HANDLE};
            use windows::Win32::Graphics::Direct3D12::ID3D12Resource;

            if frame.handle.is_null() {
                return Err(ImportError::InvalidFrame("D3D shared handle is null"));
            }
            if frame.size.width == 0 || frame.size.height == 0 {
                return Err(ImportError::InvalidFrame(
                    "D3D shared texture has zero size",
                ));
            }
            if ctx.backend != InteropBackend::Dx12 {
                return Err(ImportError::BackendMismatch {
                    frame: NativeFrameKind::Dx12SharedTexture,
                    wgpu: ctx.backend,
                });
            }

            struct OwnedHandle(HANDLE);
            impl Drop for OwnedHandle {
                fn drop(&mut self) {
                    if !self.0.is_invalid() {
                        unsafe {
                            let _ = CloseHandle(self.0);
                        }
                    }
                }
            }

            let owned = OwnedHandle(HANDLE(frame.handle));
            let texture = unsafe {
                let hal_device = ctx.device.as_hal::<wgpu::wgc::api::Dx12>().ok_or(
                    ImportError::BackendMismatch {
                        frame: NativeFrameKind::Dx12SharedTexture,
                        wgpu: ctx.backend,
                    },
                )?;

                let d3d_device = hal_device.raw_device().clone();
                let mut resource: Option<ID3D12Resource> = None;
                d3d_device
                    .OpenSharedHandle(owned.0, &mut resource)
                    .map_err(|err| ImportError::D3d12OpenShared(err.to_string()))?;
                let resource = resource.ok_or_else(|| {
                    ImportError::D3d12OpenShared("OpenSharedHandle returned null".into())
                })?;

                let hal_texture = wgpu_hal::dx12::Device::texture_from_raw(
                    resource,
                    frame.format,
                    wgpu::TextureDimension::D2,
                    wgpu::Extent3d {
                        width: frame.size.width,
                        height: frame.size.height,
                        depth_or_array_layers: 1,
                    },
                    1,
                    1,
                );

                ctx.device.create_texture_from_hal::<wgpu::wgc::api::Dx12>(
                    hal_texture,
                    &wgpu::TextureDescriptor {
                        label: Some("weld-cef-dx12-shared-texture-import"),
                        size: wgpu::Extent3d {
                            width: frame.size.width,
                            height: frame.size.height,
                            depth_or_array_layers: 1,
                        },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format: frame.format,
                        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC,
                        view_formats: &[],
                    },
                )
            };

            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            return Ok(ImportedTexture {
                texture,
                view,
                size: wgpu::Extent3d {
                    width: frame.size.width,
                    height: frame.size.height,
                    depth_or_array_layers: 1,
                },
                format: frame.format,
                generation: frame.generation,
            });
        }

        #[cfg(not(windows))]
        {
            let _ = (frame, ctx);
            Err(ImportError::BackendMismatch {
                frame: NativeFrameKind::Dx12SharedTexture,
                wgpu: InteropBackend::Unknown,
            })
        }
    }

    fn import_metal(
        frame: MetalTextureRef,
        ctx: &HostWgpuContext,
    ) -> Result<ImportedTexture, ImportError> {
        #[cfg(target_vendor = "apple")]
        {
            use objc2_io_surface::IOSurface;
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

            extern "C" {
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

                let io_surf = &*(frame.io_surface as *const IOSurface);
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
            return Ok(ImportedTexture {
                texture,
                view,
                size: wgpu::Extent3d {
                    width: frame.size.width,
                    height: frame.size.height,
                    depth_or_array_layers: 1,
                },
                format: frame.format,
                generation: frame.generation,
            });
        }

        #[cfg(not(target_vendor = "apple"))]
        {
            let _ = (frame, ctx);
            Err(ImportError::BackendMismatch {
                frame: NativeFrameKind::MetalTextureRef,
                wgpu: InteropBackend::Unknown,
            })
        }
    }

    fn import_vulkan(
        frame: DmaBufImage,
        ctx: &HostWgpuContext,
    ) -> Result<ImportedTexture, ImportError> {
        let _ = (frame, ctx);
        Err(ImportError::Unsupported(NativeFrameKind::DmaBufImage))
    }

    #[cfg(feature = "cpu-paint-fallback")]
    fn upload_cpu(frame: CpuBitmap, ctx: &HostWgpuContext) -> Result<ImportedTexture, ImportError> {
        let size = wgpu::Extent3d {
            width: frame.size.width,
            height: frame.size.height,
            depth_or_array_layers: 1,
        };
        let texture = ctx.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("cef_cpu_bitmap"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: frame.format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        ctx.queue.write_texture(
            texture.as_image_copy(),
            &frame.data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(frame.size.width * 4),
                rows_per_image: None,
            },
            size,
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Ok(ImportedTexture {
            texture,
            view,
            size,
            format: frame.format,
            generation: frame.generation,
        })
    }
}

#[cfg(target_vendor = "apple")]
fn wgpu_format_to_mtl(format: wgpu::TextureFormat) -> Option<objc2_metal::MTLPixelFormat> {
    use objc2_metal::MTLPixelFormat;
    match format {
        wgpu::TextureFormat::Bgra8Unorm => Some(MTLPixelFormat::BGRA8Unorm),
        wgpu::TextureFormat::Rgba8Unorm => Some(MTLPixelFormat::RGBA8Unorm),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_frame_slot_keeps_latest_frame() {
        let mut slot = PendingFrameSlot::default();
        let first = slot.next_generation();
        slot.store(NativeFrame::Dx12SharedTexture(Dx12SharedTexture {
            handle: std::ptr::null_mut(),
            size: PhysicalSize::new(10, 10),
            format: wgpu::TextureFormat::Bgra8Unorm,
            generation: first,
        }));
        let second = slot.next_generation();
        slot.store(NativeFrame::Dx12SharedTexture(Dx12SharedTexture {
            handle: std::ptr::null_mut(),
            size: PhysicalSize::new(20, 10),
            format: wgpu::TextureFormat::Bgra8Unorm,
            generation: second,
        }));

        let frame = slot.take().expect("latest frame should be present");
        assert_eq!(frame.generation(), second);
        assert_eq!(frame.size(), PhysicalSize::new(20, 10));
        assert!(!slot.has_frame());
    }
}
