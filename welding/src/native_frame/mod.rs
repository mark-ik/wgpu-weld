// Copyright 2026 Mark Alan Boykin
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0

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
            NativeFrame::Dx12SharedTexture(frame) => frame.size(),
            NativeFrame::MetalTextureRef(frame) => frame.size(),
            NativeFrame::DmaBufImage(frame) => frame.size(),
            #[cfg(feature = "cpu-paint-fallback")]
            NativeFrame::CpuBitmap(frame) => frame.size,
        }
    }

    pub fn format(&self) -> wgpu::TextureFormat {
        match self {
            NativeFrame::Dx12SharedTexture(frame) => frame.format(),
            NativeFrame::MetalTextureRef(frame) => frame.format(),
            NativeFrame::DmaBufImage(frame) => frame.format(),
            #[cfg(feature = "cpu-paint-fallback")]
            NativeFrame::CpuBitmap(frame) => frame.format,
        }
    }

    /// Pixel format metadata without exposing the selected wgpu major in the
    /// caller's type surface.
    pub fn pixel_format(&self) -> NativeFramePixelFormat {
        NativeFramePixelFormat::from_wgpu(self.format())
    }

    pub fn generation(&self) -> u64 {
        match self {
            NativeFrame::Dx12SharedTexture(frame) => frame.generation(),
            NativeFrame::MetalTextureRef(frame) => frame.generation(),
            NativeFrame::DmaBufImage(frame) => frame.generation(),
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
    /// importer converts it into an `OwnedHandle` exactly once before handing
    /// it to Graft's owned shared-resource wrapper.
    #[cfg(windows)]
    handle: std::os::windows::io::OwnedHandle,
    size: PhysicalSize<u32>,
    format: wgpu::TextureFormat,
    generation: u64,
}

/// Pixel format metadata that does not require a host to name welding's wgpu
/// feature row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum NativeFramePixelFormat {
    Rgba8Unorm,
    Rgba8UnormSrgb,
    Bgra8Unorm,
    Bgra8UnormSrgb,
    /// The frame uses a format this neutral metadata vocabulary cannot name.
    Unsupported,
}

impl NativeFramePixelFormat {
    fn from_wgpu(format: wgpu::TextureFormat) -> Self {
        match format {
            wgpu::TextureFormat::Rgba8Unorm => Self::Rgba8Unorm,
            wgpu::TextureFormat::Rgba8UnormSrgb => Self::Rgba8UnormSrgb,
            wgpu::TextureFormat::Bgra8Unorm => Self::Bgra8Unorm,
            wgpu::TextureFormat::Bgra8UnormSrgb => Self::Bgra8UnormSrgb,
            _ => Self::Unsupported,
        }
    }
}

impl Dx12SharedTexture {
    /// Build a frame from an already-owned Win32 `HANDLE`.
    #[cfg(windows)]
    pub fn from_owned_handle(
        handle: std::os::windows::io::OwnedHandle,
        size: PhysicalSize<u32>,
        format: wgpu::TextureFormat,
        generation: u64,
    ) -> Result<Self, ImportError> {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::Foundation::HANDLE;

        if HANDLE(handle.as_raw_handle()).is_invalid() {
            return Err(ImportError::InvalidFrame("D3D shared handle is invalid"));
        }

        Ok(Self {
            handle,
            size,
            format,
            generation,
        })
    }

    /// Build a frame from an owned raw Win32 `HANDLE`.
    ///
    /// # Safety
    ///
    /// `handle` must be a valid, owned Win32 handle to a shared D3D texture.
    /// After this call succeeds, this frame owns the handle and will close it
    /// unless it is moved through `into_owned_handle` or `into_raw_handle`.
    #[cfg(windows)]
    pub unsafe fn from_owned_raw_handle(
        handle: *mut std::os::raw::c_void,
        size: PhysicalSize<u32>,
        format: wgpu::TextureFormat,
        generation: u64,
    ) -> Result<Self, ImportError> {
        use std::os::windows::io::FromRawHandle;
        use windows::Win32::Foundation::HANDLE;

        if HANDLE(handle).is_invalid() {
            return Err(ImportError::InvalidFrame("D3D shared handle is invalid"));
        }
        Self::from_owned_handle(
            unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(handle) },
            size,
            format,
            generation,
        )
    }

    pub fn size(&self) -> PhysicalSize<u32> {
        self.size
    }

    pub fn format(&self) -> wgpu::TextureFormat {
        self.format
    }

    /// Pixel format without exposing the selected wgpu major in the type.
    pub fn pixel_format(&self) -> NativeFramePixelFormat {
        NativeFramePixelFormat::from_wgpu(self.format())
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Relinquish this frame's owned handle to a RAII owner.
    ///
    /// The returned `OwnedHandle` must be handed to the next ownership boundary
    /// exactly once.
    #[cfg(windows)]
    pub fn into_owned_handle(self) -> Result<std::os::windows::io::OwnedHandle, ImportError> {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::Foundation::HANDLE;

        let handle = self.handle;
        if HANDLE(handle.as_raw_handle()).is_invalid() {
            return Err(ImportError::InvalidFrame("D3D shared handle is invalid"));
        }
        Ok(handle)
    }

    /// Relinquish this frame's owned handle to a host that imports through a
    /// different GPU boundary. The receiving host must call `CloseHandle`
    /// after `OpenSharedHandle` succeeds or fails.
    #[cfg(windows)]
    pub fn into_raw_handle(self) -> *mut std::os::raw::c_void {
        use std::os::windows::io::IntoRawHandle;

        self.handle.into_raw_handle()
    }
}

#[cfg(test)]
mod pixel_format_tests {
    use super::*;

    #[test]
    fn neutral_format_names_every_cef_frame_format() {
        assert_eq!(
            NativeFramePixelFormat::from_wgpu(wgpu::TextureFormat::Rgba8Unorm),
            NativeFramePixelFormat::Rgba8Unorm
        );
        assert_eq!(
            NativeFramePixelFormat::from_wgpu(wgpu::TextureFormat::Rgba8UnormSrgb),
            NativeFramePixelFormat::Rgba8UnormSrgb
        );
        assert_eq!(
            NativeFramePixelFormat::from_wgpu(wgpu::TextureFormat::Bgra8Unorm),
            NativeFramePixelFormat::Bgra8Unorm
        );
        assert_eq!(
            NativeFramePixelFormat::from_wgpu(wgpu::TextureFormat::Bgra8UnormSrgb),
            NativeFramePixelFormat::Bgra8UnormSrgb
        );
        assert_eq!(
            NativeFramePixelFormat::from_wgpu(wgpu::TextureFormat::Depth32Float),
            NativeFramePixelFormat::Unsupported
        );
    }
}

// macOS
#[derive(Debug)]
pub struct MetalTextureRef {
    /// Retained `IOSurfaceRef`. Released when this frame is imported, replaced
    /// in the latest-frame mailbox, or otherwise dropped.
    #[cfg_attr(not(target_vendor = "apple"), allow(dead_code))]
    io_surface: *mut std::os::raw::c_void,
    size: PhysicalSize<u32>,
    format: wgpu::TextureFormat,
    generation: u64,
}

// SAFETY: this frame owns only one retained `IOSurfaceRef` plus copyable
// metadata. It stores no CEF UI-thread object and no `MTLTexture`; moving it
// only moves IOSurface retain/release custody, while Metal import creates the
// texture later on the host's Metal device.
unsafe impl Send for MetalTextureRef {}

impl MetalTextureRef {
    /// Build a frame from an already-retained `IOSurfaceRef`.
    ///
    /// # Safety
    ///
    /// `io_surface` must be a non-null IOSurface reference retained for this
    /// frame. After this call succeeds, the frame owns that retain and will
    /// release it on drop unless it is consumed by import.
    #[cfg(target_os = "macos")]
    pub unsafe fn from_retained_raw_io_surface(
        io_surface: *mut std::os::raw::c_void,
        size: PhysicalSize<u32>,
        format: wgpu::TextureFormat,
        generation: u64,
    ) -> Result<Self, ImportError> {
        if io_surface.is_null() {
            return Err(ImportError::InvalidFrame("IOSurface handle is null"));
        }
        Ok(Self {
            io_surface,
            size,
            format,
            generation,
        })
    }

    pub fn size(&self) -> PhysicalSize<u32> {
        self.size
    }

    pub fn format(&self) -> wgpu::TextureFormat {
        self.format
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }
}

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn CFRelease(cf: *const std::ffi::c_void);
}

#[cfg(target_os = "macos")]
impl Drop for MetalTextureRef {
    fn drop(&mut self) {
        if !self.io_surface.is_null() {
            // SAFETY: CEF gives each callback one retained IOSurface reference.
            // This move-only frame owns that reference until it is dropped.
            unsafe { CFRelease(self.io_surface.cast_const()) };
            self.io_surface = std::ptr::null_mut();
        }
    }
}

/// One plane of a DMABUF-backed image.
///
/// The plane is copyable metadata only. `buffer_index` selects the owned
/// descriptor in [`DmaBufImage`]'s buffer table, so dropping a standalone plane
/// never owns or leaks a file descriptor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DmaBufPlane {
    /// Index into `DmaBufImage`'s owned buffer table.
    buffer_index: usize,
    /// Byte offset into the dmabuf where the plane data starts.
    offset: u64,
    /// Byte size of the plane.
    size: u64,
    /// Row stride in bytes.
    stride: u32,
}

impl DmaBufPlane {
    pub fn new(buffer_index: usize, offset: u64, size: u64, stride: u32) -> Self {
        Self {
            buffer_index,
            offset,
            size,
            stride,
        }
    }

    pub fn buffer_index(&self) -> usize {
        self.buffer_index
    }

    pub fn offset(&self) -> u64 {
        self.offset
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn stride(&self) -> u32 {
        self.stride
    }
}

// Linux
#[derive(Debug)]
pub struct DmaBufImage {
    #[cfg(target_os = "linux")]
    buffers: Vec<std::os::fd::OwnedFd>,
    planes: Vec<DmaBufPlane>,
    size: PhysicalSize<u32>,
    format: wgpu::TextureFormat,
    drm_format: u32,
    modifier: u64,
    generation: u64,
}

impl DmaBufImage {
    pub fn planes(&self) -> &[DmaBufPlane] {
        &self.planes
    }

    pub fn size(&self) -> PhysicalSize<u32> {
        self.size
    }

    pub fn format(&self) -> wgpu::TextureFormat {
        self.format
    }

    pub fn drm_format(&self) -> u32 {
        self.drm_format
    }

    pub fn modifier(&self) -> u64 {
        self.modifier
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Move the owned buffer table and plane metadata to the Linux importer.
    #[cfg(target_os = "linux")]
    pub(crate) fn into_owned_parts(self) -> (Vec<std::os::fd::OwnedFd>, Vec<DmaBufPlane>) {
        (self.buffers, self.planes)
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

#[cfg(target_os = "linux")]
pub use vulkan_dmabuf::build_dmabuf_capable_device;

#[cfg(windows)]
pub use dx12::D3d11CallbackFrameCopier;

// ── Importer ─────────────────────────────────────────────────────────────────

/// Converts a [`NativeFrame`] into an [`ImportedTexture`] bound to the host's
/// wgpu device. The import path is chosen by the frame kind and the detected
/// backend in [`HostWgpuContext`]. Each platform arm lives in its own module;
/// the arms for other platforms return a typed error rather than compiling out.
pub struct WgpuTextureImporter;

// needless_return: platform-dispatch bodies end in `return X;` because a
// cfg-gated other-platform arm follows; on the matching platform the return
// looks needless to clippy but the idiom requires it.
#[allow(clippy::needless_return)]
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

    fn empty_dmabuf_frame(size: PhysicalSize<u32>, generation: u64) -> NativeFrame {
        NativeFrame::DmaBufImage(DmaBufImage {
            #[cfg(target_os = "linux")]
            buffers: Vec::new(),
            planes: Vec::new(),
            size,
            format: wgpu::TextureFormat::Bgra8Unorm,
            drm_format: 0,
            modifier: 0,
            generation,
        })
    }

    #[test]
    fn pending_frame_slot_keeps_latest_frame() {
        let mut slot = PendingFrameSlot::default();
        let first = slot.next_generation();
        slot.store(empty_dmabuf_frame(PhysicalSize::new(10, 10), first));
        let second = slot.next_generation();
        slot.store(empty_dmabuf_frame(PhysicalSize::new(20, 10), second));

        let frame = slot.take().expect("latest frame should be present");
        assert_eq!(frame.generation(), second);
        assert_eq!(frame.size(), PhysicalSize::new(20, 10));
        assert!(!slot.has_frame());
    }

    #[cfg(windows)]
    #[test]
    fn dx12_frame_into_owned_handle_keeps_one_raii_owner() {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::{
            Foundation::{DUPLICATE_SAME_ACCESS, DuplicateHandle, GetHandleInformation, HANDLE},
            System::Threading::GetCurrentProcess,
        };

        let process = unsafe { GetCurrentProcess() };
        let mut duplicated = HANDLE::default();
        unsafe {
            DuplicateHandle(
                process,
                process,
                process,
                &mut duplicated,
                0,
                false,
                DUPLICATE_SAME_ACCESS,
            )
        }
        .expect("duplicating a process handle should work");

        let frame = unsafe {
            Dx12SharedTexture::from_owned_raw_handle(
                duplicated.0,
                PhysicalSize::new(10, 10),
                wgpu::TextureFormat::Bgra8Unorm,
                1,
            )
        }
        .expect("duplicated handle should become a frame");
        let owned = frame
            .into_owned_handle()
            .expect("duplicated handle should become OwnedHandle");
        let raw = HANDLE(owned.as_raw_handle());

        let mut flags = 0;
        unsafe { GetHandleInformation(raw, &mut flags) }.expect("owned handle should remain live");

        drop(owned);

        assert!(
            unsafe { GetHandleInformation(raw, &mut flags) }.is_err(),
            "dropping the OwnedHandle should close exactly one transferred handle"
        );
    }
}
