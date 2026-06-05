use dpi::PhysicalSize;

use crate::{
    error::WeldError,
    native_frame::{HostWgpuContext, ImportedTexture},
};

/// How CEF can participate in a host compositor on the current platform.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum CefSurfaceMode {
    /// CEF can produce GPU-importable frames via `OnAcceleratedPaint`.
    /// Platform adapters copy or retain callback-scoped resources as needed
    /// before exposing host-owned textures to wgpu.
    AcceleratedPaint,
    /// CPU-bitmap fallback via `OnPaint` (`feature = "cpu-paint-fallback"`).
    /// Available regardless of GPU support; requires a texture upload per frame.
    #[cfg(feature = "cpu-paint-fallback")]
    CpuPaint,
    /// No viable surface path (missing CEF GPU support or unsupported OS).
    Unsupported,
}

/// Capability probe result for the current platform + CEF configuration.
#[derive(Debug)]
pub struct CefSurfaceCapabilities {
    pub preferred_mode: CefSurfaceMode,
    pub accelerated_paint_available: bool,
    pub cpu_paint_available: bool,
}

impl CefSurfaceCapabilities {
    /// Probe capabilities for the current platform.
    ///
    /// `accelerated_paint_available` reflects whether CEF was built with GPU
    /// support and `windowless_rendering_enabled` is set. This can only be
    /// definitively confirmed after creating a browser and observing whether
    /// `OnAcceleratedPaint` fires; this probe returns the best static estimate.
    pub fn probe() -> Self {
        let accelerated_paint_available = cfg!(target_os = "windows");
        let cpu_paint_available = cfg!(feature = "cpu-paint-fallback");
        let preferred_mode = if accelerated_paint_available {
            CefSurfaceMode::AcceleratedPaint
        } else {
            #[cfg(feature = "cpu-paint-fallback")]
            {
                CefSurfaceMode::CpuPaint
            }
            #[cfg(not(feature = "cpu-paint-fallback"))]
            {
                CefSurfaceMode::Unsupported
            }
        };

        Self {
            preferred_mode,
            accelerated_paint_available,
            cpu_paint_available,
        }
    }
}

/// Configuration for a single CEF browser surface.
pub struct CefSurfaceConfig {
    pub initial_url: String,
    pub initial_size: PhysicalSize<u32>,
    /// Render the page with a transparent background.
    pub transparent: bool,
    /// Prefer `OnAcceleratedPaint` over `OnPaint`. If accelerated paint is
    /// unavailable and `cpu-paint-fallback` is enabled, falls back automatically.
    pub prefer_accelerated: bool,
    /// Persistent user-data directory for cookies, storage, etc.
    /// `None` = in-memory / incognito.
    pub user_data_dir: Option<std::path::PathBuf>,
}

impl Default for CefSurfaceConfig {
    fn default() -> Self {
        CefSurfaceConfig {
            initial_url: "about:blank".into(),
            initial_size: PhysicalSize::new(800, 600),
            transparent: false,
            prefer_accelerated: true,
            user_data_dir: None,
        }
    }
}

/// The main abstraction for a weld-managed CEF browser surface.
///
/// Platform implementations — [`crate::windows_cef::WindowsCefProducer`],
/// [`crate::macos_cef::MacosCefProducer`], [`crate::linux_cef::LinuxCefProducer`]
/// — all implement this trait.
///
/// For single-platform code, use the [`crate::PlatformCefProducer`] alias.
pub trait CefSurfaceProducer: Send {
    fn surface_mode(&self) -> CefSurfaceMode;

    /// Acquire the most recently painted frame as a wgpu texture.
    /// Returns `Ok(None)` if no new frame is available since the last call.
    fn acquire_frame(
        &mut self,
        ctx: &HostWgpuContext,
    ) -> Result<Option<ImportedTexture>, WeldError>;

    fn resize(&mut self, size: PhysicalSize<u32>) -> Result<(), WeldError>;

    fn navigate_to_url(&mut self, url: &str) -> Result<(), WeldError>;
    fn navigate_to_string(&mut self, content: &str, mime_type: &str) -> Result<(), WeldError>;
    fn reload(&mut self) -> Result<(), WeldError>;
    fn stop(&mut self) -> Result<(), WeldError>;
    fn go_back(&mut self) -> Result<(), WeldError>;
    fn go_forward(&mut self) -> Result<(), WeldError>;

    fn send_mouse_input(&mut self, event: MouseEvent) -> Result<(), WeldError>;
    fn send_keyboard_input(&mut self, event: KeyEvent) -> Result<(), WeldError>;
    fn move_focus(&mut self, direction: FocusDirection) -> Result<(), WeldError>;

    fn post_web_message(&mut self, message: &str) -> Result<(), WeldError>;
    fn poll_web_message(&mut self) -> Option<String>;

    fn poll_navigation_event(&mut self) -> Option<NavigationEvent>;

    /// Execute a JavaScript expression in the browser's main frame. CEF
    /// provides this natively (`cef_frame_t::execute_java_script`) without
    /// requiring a CDP round-trip.
    fn execute_script(&mut self, script: &str, source_url: &str) -> Result<(), WeldError>;

    fn open_devtools(&self) -> Result<(), WeldError>;

    /// CEF-internal browser identifier. Useful for routing multi-browser
    /// callback events in the `CefClient` vtable.
    fn browser_id(&self) -> i32;

    fn close(&mut self) -> Result<(), WeldError>;
}

// ── Input event types ────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct MouseEvent {
    pub x: i32,
    pub y: i32,
    pub button: MouseButton,
    pub action: MouseAction,
    pub modifiers: EventModifiers,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseAction {
    Pressed,
    Released,
    Moved,
    WheelScrolled { delta_x: i32, delta_y: i32 },
}

#[derive(Clone, Debug)]
pub struct KeyEvent {
    pub kind: KeyEventKind,
    pub windows_key_code: i32,
    pub native_key_code: i32,
    pub character: Option<char>,
    pub modifiers: EventModifiers,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyEventKind {
    RawKeyDown,
    KeyDown,
    KeyUp,
    Char,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct EventModifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub meta: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FocusDirection {
    Forward,
    Backward,
}

// ── Navigation event ─────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum NavigationEvent {
    LoadStart {
        url: String,
    },
    LoadEnd {
        url: String,
        http_status: i32,
    },
    LoadError {
        url: String,
        error_code: i32,
        error_text: String,
    },
    TitleChanged {
        title: String,
    },
    AddressChanged {
        url: String,
    },
    /// CEF browser process terminated unexpectedly.
    ContentProcessTerminated,
    NewWindowRequested {
        url: String,
        user_gesture: bool,
    },
    ConsoleMessage {
        level: i32,
        message: String,
        source: String,
        line: i32,
    },
}
