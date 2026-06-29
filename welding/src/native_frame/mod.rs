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
#[derive(Clone, Copy, Debug)]
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

/// Copies callback-scoped CEF D3D11 textures into weld-owned shared textures
/// before opening them on the host's D3D12 device.
#[cfg(windows)]
pub struct D3d11CallbackFrameCopier {
    device: windows::Win32::Graphics::Direct3D11::ID3D11Device,
    device1: windows::Win32::Graphics::Direct3D11::ID3D11Device1,
    context: windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
}

#[cfg(windows)]
impl D3d11CallbackFrameCopier {
    pub fn new(ctx: &HostWgpuContext) -> Result<Self, ImportError> {
        use windows::{
            core::Interface,
            Win32::{
                Foundation::HMODULE,
                Graphics::{
                    Direct3D::{
                        D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL, D3D_FEATURE_LEVEL_11_0,
                        D3D_FEATURE_LEVEL_11_1,
                    },
                    Direct3D11::{
                        D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDevice,
                        ID3D11Device, ID3D11Device1, ID3D11DeviceContext,
                    },
                    Dxgi::{CreateDXGIFactory1, IDXGIAdapter, IDXGIFactory4},
                },
            },
        };

        if ctx.backend != InteropBackend::Dx12 {
            return Err(ImportError::BackendMismatch {
                frame: NativeFrameKind::Dx12SharedTexture,
                wgpu: ctx.backend,
            });
        }

        let adapter = unsafe {
            let hal_device = ctx.device.as_hal::<wgpu::wgc::api::Dx12>().ok_or(
                ImportError::BackendMismatch {
                    frame: NativeFrameKind::Dx12SharedTexture,
                    wgpu: ctx.backend,
                },
            )?;
            let luid = hal_device.raw_device().GetAdapterLuid();
            let factory = CreateDXGIFactory1::<IDXGIFactory4>()
                .map_err(|err| ImportError::Hal(format!("CreateDXGIFactory1 failed: {err}")))?;
            factory
                .EnumAdapterByLuid::<IDXGIAdapter>(luid)
                .map_err(|err| ImportError::Hal(format!("EnumAdapterByLuid failed: {err}")))?
        };

        let mut device: Option<ID3D11Device> = None;
        let mut context: Option<ID3D11DeviceContext> = None;
        let mut feature_level = D3D_FEATURE_LEVEL::default();
        let feature_levels = [D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0];
        unsafe {
            D3D11CreateDevice(
                Some(&adapter),
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&feature_levels),
                D3D11_SDK_VERSION,
                Some(&mut device),
                Some(&mut feature_level),
                Some(&mut context),
            )
        }
        .map_err(|err| ImportError::Hal(format!("D3D11CreateDevice failed: {err}")))?;
        let device =
            device.ok_or_else(|| ImportError::Hal("D3D11CreateDevice returned no device".into()))?;
        let context = context
            .ok_or_else(|| ImportError::Hal("D3D11CreateDevice returned no context".into()))?;
        let device1 = device
            .cast::<ID3D11Device1>()
            .map_err(|err| ImportError::Hal(format!("ID3D11Device1 cast failed: {err}")))?;

        Ok(Self {
            device,
            device1,
            context,
        })
    }

    fn copy_to_owned_shared_frame(
        &self,
        frame: Dx12SharedTexture,
    ) -> Result<(Dx12SharedTexture, windows::Win32::Graphics::Direct3D11::ID3D11Texture2D), ImportError>
    {
        use windows::{
            core::{Interface, PCWSTR},
            Win32::{
                Foundation::{CloseHandle, GENERIC_ALL, HANDLE},
                Graphics::{
                    Direct3D11::{
                        D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE,
                        D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX, D3D11_RESOURCE_MISC_SHARED_NTHANDLE,
                        D3D11_USAGE_DEFAULT, ID3D11Texture2D,
                    },
                    Dxgi::{IDXGIKeyedMutex, IDXGIResource1},
                },
            },
        };

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

        let source_handle = OwnedHandle(HANDLE(frame.handle));
        let source = unsafe { self.device1.OpenSharedResource1::<ID3D11Texture2D>(source_handle.0) }
            .map_err(|err| ImportError::D3d11OpenShared(err.to_string()))?;
        let mut desc = Default::default();
        unsafe { source.GetDesc(&mut desc) };
        desc.Usage = D3D11_USAGE_DEFAULT;
        desc.BindFlags = (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32;
        desc.CPUAccessFlags = 0;
        desc.MiscFlags =
            (D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX.0 | D3D11_RESOURCE_MISC_SHARED_NTHANDLE.0) as u32;

        let mut target = None;
        unsafe { self.device.CreateTexture2D(&desc, None, Some(&mut target)) }
            .map_err(|err| ImportError::Hal(format!("D3D11 CreateTexture2D failed: {err}")))?;
        let target =
            target.ok_or_else(|| ImportError::Hal("D3D11 CreateTexture2D returned null".into()))?;
        let target_mutex = target
            .cast::<IDXGIKeyedMutex>()
            .map_err(|err| ImportError::Hal(format!("IDXGIKeyedMutex cast failed: {err}")))?;
        unsafe { target_mutex.AcquireSync(0, 500) }
            .map_err(|err| ImportError::Hal(format!("IDXGIKeyedMutex AcquireSync failed: {err}")))?;
        unsafe {
            self.context.CopyResource(&target, &source);
        }
        let copy_result = self.flush_and_wait_for_gpu();
        let release_result = unsafe { target_mutex.ReleaseSync(0) }
            .map_err(|err| ImportError::Hal(format!("IDXGIKeyedMutex ReleaseSync failed: {err}")));
        copy_result?;
        release_result?;

        let dxgi_resource = target
            .cast::<IDXGIResource1>()
            .map_err(|err| ImportError::Hal(format!("IDXGIResource1 cast failed: {err}")))?;
        let target_handle = unsafe {
            dxgi_resource
                .CreateSharedHandle(None, GENERIC_ALL.0, PCWSTR::null())
                .map_err(|err| ImportError::Hal(format!("DXGI CreateSharedHandle failed: {err}")))?
        };
        Ok((
            Dx12SharedTexture {
                handle: target_handle.0,
                ..frame
            },
            target,
        ))
    }

    fn flush_and_wait_for_gpu(&self) -> Result<(), ImportError> {
        use windows::Win32::Graphics::Direct3D11::{D3D11_QUERY_DESC, D3D11_QUERY_EVENT};

        let mut query = None;
        unsafe {
            self.device
                .CreateQuery(
                    &D3D11_QUERY_DESC {
                        Query: D3D11_QUERY_EVENT,
                        MiscFlags: 0,
                    },
                    Some(&mut query),
                )
                .map_err(|err| ImportError::Hal(format!("D3D11 CreateQuery failed: {err}")))?;
        }
        let query =
            query.ok_or_else(|| ImportError::Hal("D3D11 CreateQuery returned null".into()))?;
        unsafe {
            self.context.End(&query);
            self.context.Flush();
        }

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            let mut data: u32 = 0;
            let result = unsafe {
                self.context.GetData(
                    &query,
                    Some(&mut data as *mut _ as *mut std::ffi::c_void),
                    std::mem::size_of::<u32>() as u32,
                    0,
                )
            };
            if result.is_ok() {
                return Ok(());
            }
            if std::time::Instant::now() > deadline {
                return Err(ImportError::Hal(
                    "D3D11 GPU copy timed out after 2 seconds".into(),
                ));
            }
            std::thread::yield_now();
        }
    }

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

    /// Copy a callback-scoped Windows CEF frame into an application-owned
    /// texture before returning the CEF surface to its pool.
    ///
    /// CEF permits opening the shared handle inside `OnAcceleratedPaint`, but
    /// the opened resource must not escape that callback. Copying through D3D11
    /// matches CEF's native Windows sharing path and keeps the pooled source
    /// inside the callback.
    #[cfg(windows)]
    pub fn copy_dx12_callback_frame(
        frame: Dx12SharedTexture,
        ctx: &HostWgpuContext,
        copier: &D3d11CallbackFrameCopier,
    ) -> Result<ImportedTexture, ImportError> {
        let (copied_frame, _d3d11_target) = copier.copy_to_owned_shared_frame(frame)?;
        let imported = Self::import_dx12(copied_frame, ctx)?;
        let cache_flush_buffer = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("weld-cef-dx12-cache-flush"),
            size: wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as u64,
            usage: wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut encoder = ctx.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("weld-cef-dx12-cache-flush"),
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &imported.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &cache_flush_buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT),
                    rows_per_image: Some(1),
                },
            },
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        let submission = ctx.queue.submit([encoder.finish()]);
        ctx.device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(submission),
                timeout: None,
            })
            .map_err(|err| {
                ImportError::Hal(format!("waiting for D3D12 cache flush failed: {err}"))
            })?;
        Ok(imported)
    }

    fn import_dx12(
        frame: Dx12SharedTexture,
        ctx: &HostWgpuContext,
    ) -> Result<ImportedTexture, ImportError> {
        #[cfg(windows)]
        {
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

            // Delegate the generic OpenSharedHandle -> wgpu import to grafting (the
            // shared interop core). welding's CEF-specific callback copy + cache-flush
            // stay in `copy_dx12_callback_frame`, which calls this on the copied,
            // owned shared handle.
            let g_host = grafting::HostWgpuContext::new(ctx.device.clone(), ctx.queue.clone());
            let g_frame = grafting::Dx12SharedTexture {
                handle: frame.handle,
                size: frame.size,
                format: frame.format,
                generation: frame.generation,
                // The handle is an already-synced owned copy; the low-level import
                // ignores these sync fields (they drive grafting's high-level
                // WgpuTextureImporter, which welding does not use here).
                producer_sync: grafting::SyncMechanism::ImplicitGlFlush,
                fence_value: 0,
            };
            let texture = grafting::import_dx12_shared_texture(&g_frame, &g_host)
                .map_err(|err| ImportError::D3d12OpenShared(err.to_string()))?;

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
        #[cfg(target_os = "linux")]
        {
            return import_dmabuf_vulkan(frame, ctx);
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

// ── Linux DMABUF → Vulkan import ──────────────────────────────────────────────
//
// Ported from `wgpu-graft/grafting/src/vulkan_dmabuf.rs`. For Phase 4 first cut
// we accept only single-plane formats (BGRA8 / RGBA8 from CEF). Multi-plane
// (e.g. NV12 video) deferred until CEF actually emits it on this path.

#[cfg(target_os = "linux")]
fn import_dmabuf_vulkan(
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
    let drm_modifier = frame.modifier;

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
            .usage(vk::ImageUsageFlags::SAMPLED)
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
        let imported = ctx
            .device
            .create_texture_from_hal::<wgpu::wgc::api::Vulkan>(
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
                        usage: wgpu::TextureUses::RESOURCE,
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
                    usage: wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                },
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

#[cfg(target_os = "linux")]
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
