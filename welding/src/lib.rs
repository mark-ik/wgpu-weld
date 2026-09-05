// Copyright 2026 Mark Alan Boykin
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0

#![doc = include_str!("../README.md")]
// The subprocess-tax doc examples exist to show WHERE in `main` the call must
// sit, so their `fn main` wrapper is the point, not boilerplate.
#![allow(clippy::needless_doctest_main)]

// Alias the feature-selected wgpu family back to the plain crate names. Public
// re-exports let hosts name the exact Device/Texture types welding expects.
#[cfg(feature = "wgpu-30")]
pub extern crate wgpu_30 as wgpu;
#[cfg(feature = "wgpu-30")]
pub extern crate wgpu_hal_30 as wgpu_hal;

#[cfg(all(feature = "wgpu-29", not(feature = "wgpu-30")))]
pub extern crate wgpu_29 as wgpu;
#[cfg(all(feature = "wgpu-29", not(feature = "wgpu-30")))]
pub extern crate wgpu_hal_29 as wgpu_hal;

#[cfg(all(
    feature = "wgpu-28",
    not(feature = "wgpu-29"),
    not(feature = "wgpu-30")
))]
pub extern crate wgpu_28 as wgpu;
#[cfg(all(
    feature = "wgpu-28",
    not(feature = "wgpu-29"),
    not(feature = "wgpu-30")
))]
pub extern crate wgpu_hal_28 as wgpu_hal;

#[cfg(not(any(feature = "wgpu-28", feature = "wgpu-29", feature = "wgpu-30")))]
compile_error!(
    "welding needs one wgpu version feature: enable `wgpu-29` (default), `wgpu-30`, or `wgpu-28`"
);

// File-descriptor numbers are process-wide and may be reused immediately after
// close. Tests which assert descriptor ownership must share one lock across
// modules or a parallel test can make a closed descriptor appear live again.
#[cfg(all(test, target_os = "linux"))]
pub(crate) fn lock_fd_table() -> std::sync::MutexGuard<'static, ()> {
    static FD_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    FD_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub mod error;
pub mod native_frame;
pub mod runtime;
pub mod surface;

#[cfg(target_os = "macos")]
mod wgpu_compat;

// Only the producers consume this, and they are cef-runtime-only. The logic is
// worth unit-testing either way, so allow rather than cfg the module out.
#[cfg_attr(not(feature = "cef-runtime"), allow(dead_code))]
pub mod app;
#[cfg_attr(not(feature = "cef-runtime"), allow(dead_code))]
mod auth;
#[cfg_attr(not(feature = "cef-runtime"), allow(dead_code))]
mod cookies;
#[cfg_attr(not(feature = "cef-runtime"), allow(dead_code))]
mod cursor;
#[cfg_attr(not(feature = "cef-runtime"), allow(dead_code))]
mod devtools;
#[cfg_attr(not(feature = "cef-runtime"), allow(dead_code))]
mod downloads;
#[cfg_attr(not(feature = "cef-runtime"), allow(dead_code))]
mod ime;
#[cfg_attr(not(feature = "cef-runtime"), allow(dead_code))]
mod permissions;
#[cfg_attr(not(feature = "cef-runtime"), allow(dead_code))]
mod popup;
#[cfg_attr(not(feature = "cef-runtime"), allow(dead_code))]
mod view;

#[cfg(feature = "cef-runtime")]
mod cef_input;
#[cfg(feature = "cef-runtime")]
mod drag;
#[cfg(feature = "cef-runtime")]
mod profile;
#[cfg(feature = "cef-runtime")]
mod snapshot;

#[cfg(windows)]
pub mod windows_cef;

#[cfg(target_os = "macos")]
pub mod macos_cef;

#[cfg(target_os = "linux")]
pub mod linux_cef;

// ── Platform aliases (mirrors wgpu-scry's PlatformWebSurfaceProducer pattern) ─

#[cfg(windows)]
pub use windows_cef::{
    WindowsCefConfig as PlatformCefConfig, WindowsCefProducer as PlatformCefProducer,
};

#[cfg(target_os = "macos")]
pub use macos_cef::{MacosCefConfig as PlatformCefConfig, MacosCefProducer as PlatformCefProducer};

#[cfg(target_os = "linux")]
pub use linux_cef::{LinuxCefConfig as PlatformCefConfig, LinuxCefProducer as PlatformCefProducer};

// ── Flat re-exports ───────────────────────────────────────────────────────────

pub use auth::AuthId;
pub use downloads::DownloadId;
pub use error::WeldError;
#[cfg(target_os = "linux")]
pub use native_frame::build_dmabuf_capable_device;
pub use native_frame::{
    HostWgpuContext, ImportError, ImportedTexture, InteropBackend, NativeFrame, NativeFrameKind,
    WgpuTextureImporter,
};
pub use permissions::{PermissionId, PermissionKind};
#[cfg(all(feature = "cef-runtime", target_os = "windows"))]
pub use runtime::CefWindowsSandboxContext;
pub use runtime::{CefLogSeverity, CefRuntime, CefRuntimeConfig, CefSandboxMode};
pub use surface::{
    BrowserFeatureStatus, CefSurfaceCapabilities, CefSurfaceConfig, CefSurfaceEvent,
    CefSurfaceMode, CefSurfaceProducer, ContactDevice, ContextMenuTarget, Cookie, CursorShape,
    DragEventKind, DragFile, DragInput, DragOperations, DragPayload, EventModifiers,
    FocusDirection, ImeComposition, KeyEvent, KeyEventKind, MouseAction, MouseButton, MouseEvent,
    NavigationEvent, PopupRect, PopupSurface, ProcessTerminationStatus, SameSite,
    SnapshotPngCompletion, SnapshotRequestId, TouchInput, TouchPhase, WebRequestId, ZoomCommand,
};
