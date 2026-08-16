//! Native GPU surface handles produced by CEF's `OnAcceleratedPaint` callback,
//! plus the wgpu import infrastructure.
//!
//! This is the CEF-shaped analogue of `wgpu-scry::native_frame` and
//! `wgpu-graft::grafting`: the producer gives us a borrowed native resource in
//! the paint callback, `weld` copies or retains that resource immediately, and
//! the host later receives an owned texture bound to its `wgpu::Device`.

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

/// A GPU surface resource originating from CEF's `OnAcceleratedPaint`
/// callback, ready to be imported into the host's wgpu pipeline.
#[derive(Debug)]
#[non_exhaustive]
pub enum NativeFrame {
    /// Windows: shared handle to a weld-owned D3D11 texture copied from CEF's
    /// pooled resource inside `OnAcceleratedPaint`.
    /// Imported via the D3D12 open-shared path.
    ///
    /// # Handle lifetime
    /// CEF's source `HANDLE` and pooled resource are used only inside
    /// `OnAcceleratedPaint`. This frame owns the handle to the copied texture.
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
#[derive(Debug)]
pub struct Dx12SharedTexture {
    /// Owned Win32 `HANDLE` to a shared D3D texture.
    ///
    /// CEF's callback-scoped handle is never stored directly. The Windows
    /// callback copier produces an application-owned shared texture, and the
    /// importer closes its handle after opening the D3D12 resource.
    pub handle: *mut std::os::raw::c_void,
    pub size: PhysicalSize<u32>,
    pub format: wgpu::TextureFormat,
    pub generation: u64,
}

unsafe impl Send for Dx12SharedTexture {}

impl Dx12SharedTexture {
    /// Relinquish this frame's owned handle to a host that imports through a
    /// different GPU boundary. The receiving host must call `CloseHandle`
    /// after `OpenSharedHandle` succeeds or fails.
    pub fn into_raw_handle(self) -> *mut std::os::raw::c_void {
        let frame = std::mem::ManuallyDrop::new(self);
        frame.handle
    }
}

/// A CEF callback frame owns the duplicate shared handle returned by the
/// callback copier. Keeping that ownership on the frame closes dropped
/// mailbox replacements instead of leaking one Win32 handle per paint.
#[cfg(windows)]
impl Drop for Dx12SharedTexture {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe {
                let _ = windows::Win32::Foundation::CloseHandle(
                    windows::Win32::Foundation::HANDLE(self.handle),
                );
            }
            self.handle = std::ptr::null_mut();
        }
    }
}

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

/// One plane of a DMABUF-backed image. Field widths match CEF's
/// `cef_accelerated_paint_native_pixmap_plane_t`.
#[derive(Copy, Clone, Debug)]
pub struct DmaBufPlane {
    /// Owned DMABUF file descriptor. `welding` `dup(2)`s callback-scoped FDs
    /// before storing them here. The Vulkan importer takes ownership on
    /// import (Vulkan closes the fd internally); otherwise `DmaBufImage::Drop`
    /// closes the fd.
    pub fd: i32,
    /// Byte offset into the dmabuf where the plane data starts.
    pub offset: u64,
    /// Byte size of the plane.
    pub size: u64,
    /// Row stride in bytes.
    pub stride: u32,
}

// Linux
#[derive(Debug)]
pub struct DmaBufImage {
    pub planes: Vec<DmaBufPlane>,
    pub size: PhysicalSize<u32>,
    pub format: wgpu::TextureFormat,
    pub drm_format: u32,
    pub modifier: u64,
    pub generation: u64,
}

impl DmaBufImage {
    /// Release ownership of the contained fds without closing them. Used by
    /// the Vulkan importer immediately before it hands the fds to
    /// `vkAllocateMemory`, which takes ownership and closes them on its own.
    #[cfg(target_os = "linux")]
    pub(crate) fn forget_fds(mut self) -> Vec<DmaBufPlane> {
        std::mem::take(&mut self.planes)
    }
}

#[cfg(target_os = "linux")]
impl Drop for DmaBufImage {
    fn drop(&mut self) {
        for plane in &self.planes {
            if plane.fd >= 0 {
                // Safety: we own the fd (dup'd in OnAcceleratedPaint).
                unsafe {
                    libc::close(plane.fd);
                }
            }
        }
    }
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
#[derive(Clone)]
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

// ── Platform import paths ─────────────────────────────────────────────────────

#[cfg(windows)]
mod dx12;
#[cfg(target_vendor = "apple")]
mod metal;
#[cfg(target_os = "linux")]
mod vulkan_dmabuf;

#[cfg(windows)]
pub use dx12::D3d11CallbackFrameCopier;

// ── Importer ─────────────────────────────────────────────────────────────────

/// Converts a [`NativeFrame`] into an [`ImportedTexture`] bound to the host's
/// wgpu device. The import path is chosen by the frame kind and the detected
/// backend in [`HostWgpuContext`]. Each platform arm lives in its own module;
/// the arms for other platforms return a typed error rather than compiling out.
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
            return dx12::import_dx12(frame, ctx);
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
            return metal::import_metal(frame, ctx);
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
        #[cfg(target_os = "linux")]
        {
            return vulkan_dmabuf::import_vulkan(frame, ctx);
        }

        #[cfg(not(target_os = "linux"))]
        {
            let _ = (frame, ctx);
            Err(ImportError::Unsupported(NativeFrameKind::DmaBufImage))
        }
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
