// Copyright 2026 Mark Alan Boykin
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0

//! Windows: callback-time D3D11 copy of CEF's pooled shared texture, then
//! the D3D12 open-shared import (delegated to `grafting`).
//!
//! Split out of `native_frame/mod.rs`.

use super::*;

/// Copies callback-scoped CEF D3D11 textures into weld-owned shared textures
/// before opening them on the host's D3D12 device.
pub struct D3d11CallbackFrameCopier {
    device: windows::Win32::Graphics::Direct3D11::ID3D11Device,
    device1: windows::Win32::Graphics::Direct3D11::ID3D11Device1,
    context: windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
}

impl D3d11CallbackFrameCopier {
    pub fn new(ctx: &HostWgpuContext) -> Result<Self, ImportError> {
        use windows::{
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
            core::Interface,
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
        let device = device
            .ok_or_else(|| ImportError::Hal("D3D11CreateDevice returned no device".into()))?;
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
    ) -> Result<
        (
            Dx12SharedTexture,
            windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
        ),
        ImportError,
    > {
        use windows::{
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
            core::{Interface, PCWSTR},
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

        let size = frame.size;
        let format = frame.format;
        let generation = frame.generation;
        let source_handle = OwnedHandle(HANDLE(frame.into_raw_handle()));
        let source = unsafe {
            self.device1
                .OpenSharedResource1::<ID3D11Texture2D>(source_handle.0)
        }
        .map_err(|err| ImportError::D3d11OpenShared(err.to_string()))?;
        let mut desc = Default::default();
        unsafe { source.GetDesc(&mut desc) };
        desc.Usage = D3D11_USAGE_DEFAULT;
        desc.BindFlags = (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32;
        desc.CPUAccessFlags = 0;
        desc.MiscFlags = (D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX.0
            | D3D11_RESOURCE_MISC_SHARED_NTHANDLE.0) as u32;

        let mut target = None;
        unsafe { self.device.CreateTexture2D(&desc, None, Some(&mut target)) }
            .map_err(|err| ImportError::Hal(format!("D3D11 CreateTexture2D failed: {err}")))?;
        let target =
            target.ok_or_else(|| ImportError::Hal("D3D11 CreateTexture2D returned null".into()))?;
        let target_mutex = target
            .cast::<IDXGIKeyedMutex>()
            .map_err(|err| ImportError::Hal(format!("IDXGIKeyedMutex cast failed: {err}")))?;
        unsafe { target_mutex.AcquireSync(0, 500) }.map_err(|err| {
            ImportError::Hal(format!("IDXGIKeyedMutex AcquireSync failed: {err}"))
        })?;
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
                size,
                format,
                generation,
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

impl WgpuTextureImporter {
    /// Copy a callback-scoped Windows CEF frame into an application-owned
    /// texture before returning the CEF surface to its pool.
    ///
    /// CEF permits opening the shared handle inside `OnAcceleratedPaint`, but
    /// the opened resource must not escape that callback. Copying through D3D11
    /// matches CEF's native Windows sharing path and keeps the pooled source
    /// inside the callback.
    pub fn copy_dx12_callback_frame_to_owned(
        frame: Dx12SharedTexture,
        copier: &D3d11CallbackFrameCopier,
    ) -> Result<Dx12SharedTexture, ImportError> {
        let (copied_frame, _d3d11_target) = copier.copy_to_owned_shared_frame(frame)?;
        Ok(copied_frame)
    }

    /// Import a callback-owned CEF frame and perform the cache-visible read
    /// that the D3D11-to-D3D12 handoff requires. Consumes the frame, closing
    /// its transferred Win32 handle after `OpenSharedHandle` has taken its own
    /// resource reference.
    pub fn import_owned_dx12_callback_frame(
        frame: Dx12SharedTexture,
        ctx: &HostWgpuContext,
    ) -> Result<ImportedTexture, ImportError> {
        let imported = Self::import_dx12(frame, ctx)?;
        let cache_flush_buffer = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("weld-cef-dx12-cache-flush"),
            size: wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as u64,
            usage: wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut encoder = ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
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

    /// Compatibility path for hosts that want Weld to both copy and import.
    /// New neutral surface-engine hosts call
    /// [`copy_dx12_callback_frame_to_owned`](Self::copy_dx12_callback_frame_to_owned)
    /// in the callback, then import the transferred handle themselves.
    pub fn copy_dx12_callback_frame(
        frame: Dx12SharedTexture,
        ctx: &HostWgpuContext,
        copier: &D3d11CallbackFrameCopier,
    ) -> Result<ImportedTexture, ImportError> {
        let frame = Self::copy_dx12_callback_frame_to_owned(frame, copier)?;
        Self::import_owned_dx12_callback_frame(frame, ctx)
    }
}

/// The Windows half of [`WgpuTextureImporter::import`] for
/// [`NativeFrame::Dx12SharedTexture`] frames.
pub(super) fn import_dx12(
    frame: Dx12SharedTexture,
    ctx: &HostWgpuContext,
) -> Result<ImportedTexture, ImportError> {
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
