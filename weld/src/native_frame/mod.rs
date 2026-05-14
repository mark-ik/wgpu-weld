/// Native GPU surface handles produced by CEF's `OnAcceleratedPaint` callback,
/// plus the wgpu import infrastructure.
///
/// This module mirrors `wgpu-scry::native_frame` and `wgpu-graft::grafting`'s
/// frame-import layer. The GPU import paths (D3D11 → D3D12, IOSurface → Metal,
/// DMABUF → Vulkan) are structurally identical to those in wgpu-scry; only the
/// source — how the handle is obtained — differs (CEF callback vs WGC capture /
/// ScreenCaptureKit). Implementation bodies are stubs until CEF integration is
/// active.
use wgpu;

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
        match device.get_info().backend {
            wgpu::Backend::Vulkan => InteropBackend::Vulkan,
            wgpu::Backend::Metal => InteropBackend::Metal,
            wgpu::Backend::Dx12 => InteropBackend::Dx12,
            _ => InteropBackend::Unknown,
        }
    }
}

// ── Native frame types ────────────────────────────────────────────────────────

/// A GPU surface handle emitted by CEF's `OnAcceleratedPaint` callback,
/// ready to be imported into the host's wgpu pipeline.
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
    /// Linux: DMABUF file descriptor (planned; CEF API still stabilising).
    VulkanExternalImage(VulkanExternalImage),
    /// CPU bitmap from `OnPaint` (requires `feature = "cpu-paint-fallback"`).
    #[cfg(feature = "cpu-paint-fallback")]
    CpuBitmap(CpuBitmap),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum NativeFrameKind {
    Dx12SharedTexture,
    MetalTextureRef,
    VulkanExternalImage,
    #[cfg(feature = "cpu-paint-fallback")]
    CpuBitmap,
}

impl NativeFrame {
    pub fn kind(&self) -> NativeFrameKind {
        match self {
            NativeFrame::Dx12SharedTexture(_) => NativeFrameKind::Dx12SharedTexture,
            NativeFrame::MetalTextureRef(_) => NativeFrameKind::MetalTextureRef,
            NativeFrame::VulkanExternalImage(_) => NativeFrameKind::VulkanExternalImage,
            #[cfg(feature = "cpu-paint-fallback")]
            NativeFrame::CpuBitmap(_) => NativeFrameKind::CpuBitmap,
        }
    }
}

// Windows
pub struct Dx12SharedTexture {
    /// Win32 HANDLE to a shared D3D11 texture.
    /// Either the original (valid only during OnAcceleratedPaint) or a
    /// DuplicateHandle'd copy held until import completes.
    pub handle: *mut std::os::raw::c_void,
    pub width: u32,
    pub height: u32,
}

unsafe impl Send for Dx12SharedTexture {}

// macOS
pub struct MetalTextureRef {
    /// Retained IOSurfaceRef. Released after wgpu import.
    pub io_surface: *mut std::os::raw::c_void,
    pub width: u32,
    pub height: u32,
}

unsafe impl Send for MetalTextureRef {}

// Linux
pub struct VulkanExternalImage {
    /// DMABUF file descriptor. Closed after Vulkan import.
    pub dmabuf_fd: std::os::unix::io::RawFd,
    pub width: u32,
    pub height: u32,
    pub format: u32,   // DRM fourcc
    pub modifier: u64, // DRM format modifier
}

#[cfg(feature = "cpu-paint-fallback")]
pub struct CpuBitmap {
    /// BGRA8 pixel data (matches CEF's OnPaint buffer format).
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
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
        HostWgpuContext { device, queue, backend }
    }
}

// ── Import result ─────────────────────────────────────────────────────────────

pub struct ImportedTexture {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub size: wgpu::Extent3d,
}

// ── Importer ─────────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ImportError {
    #[error("backend mismatch: frame is {frame:?} but wgpu reports {wgpu:?}")]
    BackendMismatch { frame: NativeFrameKind, wgpu: InteropBackend },
    #[error("D3D11 open-shared-resource failed: {0}")]
    D3d11OpenShared(String),
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
            NativeFrame::VulkanExternalImage(f) => Self::import_vulkan(f, ctx),
            #[cfg(feature = "cpu-paint-fallback")]
            NativeFrame::CpuBitmap(f) => Self::upload_cpu(f, ctx),
        }
    }

    fn import_dx12(
        _frame: Dx12SharedTexture,
        _ctx: &HostWgpuContext,
    ) -> Result<ImportedTexture, ImportError> {
        // Mirrors wgpu-scry's windows_capture + native_frame D3D12 path:
        // 1. ID3D11Device::OpenSharedResource1 → ID3D11Texture2D
        // 2. QueryInterface → IDXGIResource1 → CreateSharedHandle (NT handle)
        // 3. wgpu-hal Dx12: Device::create_texture_from_raw with the NT handle
        // 4. wgpu::Device::create_texture_from_hal → wgpu::Texture
        todo!("D3D11 open-shared → D3D12 import (mirrors wgpu-scry::native_frame)")
    }

    fn import_metal(
        _frame: MetalTextureRef,
        _ctx: &HostWgpuContext,
    ) -> Result<ImportedTexture, ImportError> {
        // Mirrors wgpu-scry's WKWebView macOS path:
        // 1. IOSurface::new_texture_with_descriptor on the MTLDevice
        // 2. wgpu-hal Metal: Device::texture_from_raw(Retained<ProtocolObject<dyn MTLTexture>>)
        // 3. wgpu::Device::create_texture_from_hal → wgpu::Texture
        todo!("IOSurface → MTLTexture import (mirrors wgpu-scry::native_frame)")
    }

    fn import_vulkan(
        _frame: VulkanExternalImage,
        _ctx: &HostWgpuContext,
    ) -> Result<ImportedTexture, ImportError> {
        // VK_KHR_external_memory_fd + VK_EXT_image_drm_format_modifier:
        // 1. vkCreateImage with VkExternalMemoryImageCreateInfo (DMABUF fd type)
        // 2. vkAllocateMemory with VkImportMemoryFdInfoKHR
        // 3. wgpu-hal Vulkan: Device::texture_from_raw
        todo!("DMABUF → Vulkan external memory import")
    }

    #[cfg(feature = "cpu-paint-fallback")]
    fn upload_cpu(
        frame: CpuBitmap,
        ctx: &HostWgpuContext,
    ) -> Result<ImportedTexture, ImportError> {
        let size = wgpu::Extent3d { width: frame.width, height: frame.height, depth_or_array_layers: 1 };
        let texture = ctx.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("cef_cpu_bitmap"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Bgra8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        ctx.queue.write_texture(
            texture.as_image_copy(),
            &frame.data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(frame.width * 4),
                rows_per_image: None,
            },
            size,
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Ok(ImportedTexture { texture, view, size })
    }
}
