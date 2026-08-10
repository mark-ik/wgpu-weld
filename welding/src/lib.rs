#![doc = include_str!("../README.md")]

pub mod error;
pub mod native_frame;
pub mod runtime;
pub mod surface;

// Only the producers consume this, and they are cef-runtime-only. The logic is
// worth unit-testing either way, so allow rather than cfg the module out.
#[cfg_attr(not(feature = "cef-runtime"), allow(dead_code))]
mod popup;

#[cfg(feature = "cef-runtime")]
mod cef_input;

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

pub use error::WeldError;
pub use native_frame::{
    HostWgpuContext, ImportError, ImportedTexture, InteropBackend, NativeFrame, NativeFrameKind,
    WgpuTextureImporter,
};
pub use runtime::{CefLogSeverity, CefRuntime, CefRuntimeConfig};
pub use surface::{
    BrowserFeatureStatus, CefSurfaceCapabilities, CefSurfaceConfig, CefSurfaceMode,
    CefSurfaceProducer, Cookie, EventModifiers, FocusDirection, KeyEvent, KeyEventKind,
    MouseAction, MouseButton, MouseEvent, NavigationEvent, PopupRect, PopupSurface, SameSite,
};
