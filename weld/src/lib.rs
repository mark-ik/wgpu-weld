#![doc = include_str!("../README.md")]

pub mod cef_ffi;
pub mod error;
pub mod native_frame;
pub mod runtime;
pub mod surface;

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
    CefSurfaceCapabilities, CefSurfaceConfig, CefSurfaceMode, CefSurfaceProducer, EventModifiers,
    FocusDirection, KeyEvent, KeyEventKind, MouseAction, MouseButton, MouseEvent, NavigationEvent,
};
