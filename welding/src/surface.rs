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
    pub cookies: BrowserFeatureStatus,
    pub cookie_change_events: BrowserFeatureStatus,
    pub script_execution: BrowserFeatureStatus,
    pub script_result: BrowserFeatureStatus,
    pub devtools: BrowserFeatureStatus,
    pub downloads: BrowserFeatureStatus,
    pub popups: BrowserFeatureStatus,
    pub context_menus: BrowserFeatureStatus,
    pub console_messages: BrowserFeatureStatus,
}

impl CefSurfaceCapabilities {
    /// Probe capabilities for the current platform.
    ///
    /// `accelerated_paint_available` reflects whether CEF was built with GPU
    /// support and `windowless_rendering_enabled` is set. This can only be
    /// definitively confirmed after creating a browser and observing whether
    /// `OnAcceleratedPaint` fires; this probe returns the best static estimate.
    pub fn probe() -> Self {
        // Windows, Linux and macOS have each been verified end to end against
        // a real CEF build (see the accelerated-OSR plan's phase table). The
        // remaining honesty gap is the one the doc comment describes: whether
        // *this* CEF distribution was built with GPU support can only be
        // confirmed once `OnAcceleratedPaint` actually fires.
        let accelerated_paint_available = cfg!(any(
            target_os = "windows",
            target_os = "linux",
            target_os = "macos"
        ));
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
            cookies: BrowserFeatureStatus::Unsupported("CEF cookie manager is not wired yet"),
            cookie_change_events: BrowserFeatureStatus::Unsupported(
                "CEF cookie observers are not wired yet",
            ),
            script_execution: BrowserFeatureStatus::Supported,
            script_result: BrowserFeatureStatus::Unsupported(
                "CEF result-bearing script bridge is not wired yet",
            ),
            devtools: BrowserFeatureStatus::Supported,
            downloads: BrowserFeatureStatus::Unsupported(
                "no CefDownloadHandler is registered; downloads are silently dropped",
            ),
            // Two unrelated things share this name. Widget surfaces (select
            // dropdowns, autocomplete) are rendered: see `acquire_popup`.
            // Popup *browsers* (window.open) are denied and reported as
            // NewWindowRequested, because welding renders one surface per
            // producer and has nowhere to put a second browser.
            popups: BrowserFeatureStatus::Partial(
                "widget surfaces are rendered via acquire_popup; popup browsers \
                 (window.open) are denied and surfaced as NewWindowRequested",
            ),
            context_menus: BrowserFeatureStatus::Unsupported(
                "no CefContextMenuHandler is registered; CEF's default menu is inert under OSR",
            ),
            console_messages: BrowserFeatureStatus::Supported,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrowserFeatureStatus {
    Supported,
    Unsupported(&'static str),
    Partial(&'static str),
}

#[cfg(test)]
mod capability_tests {
    use super::*;

    /// A capability report that overstates is worse than a missing feature,
    /// so every `Supported` here is pinned to a handler that exists. When a
    /// feature lands, flip the status *and* this test in the same change.
    #[test]
    fn probe_only_claims_what_is_wired() {
        let caps = CefSurfaceCapabilities::probe();

        // Wired: script execution, devtools, console messages (the display
        // handler's on_console_message), and the accelerated paint path on all
        // three verified platforms.
        assert_eq!(caps.script_execution, BrowserFeatureStatus::Supported);
        assert_eq!(caps.devtools, BrowserFeatureStatus::Supported);
        assert_eq!(caps.console_messages, BrowserFeatureStatus::Supported);
        assert!(caps.accelerated_paint_available);
        assert_eq!(caps.preferred_mode, CefSurfaceMode::AcceleratedPaint);

        // Not wired. These carry a reason string rather than a bare status so
        // a host can report *why* to its own user.
        for status in [
            caps.cookies,
            caps.cookie_change_events,
            caps.script_result,
            caps.downloads,
            caps.context_menus,
        ] {
            assert!(
                matches!(status, BrowserFeatureStatus::Unsupported(reason) if !reason.is_empty()),
                "expected an explained Unsupported, got {status:?}"
            );
        }

        // Popups are the split case: creation is handled, rendering is not.
        assert!(matches!(caps.popups, BrowserFeatureStatus::Partial(_)));
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SameSite {
    Strict,
    Lax,
    None,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Cookie {
    pub name: String,
    pub value: String,
    pub domain: String,
    pub path: String,
    pub secure: bool,
    pub http_only: bool,
    pub same_site: Option<SameSite>,
    pub expires: Option<f64>,
    pub partitioned: bool,
}

// ── Popup widget surfaces ────────────────────────────────────────────────────

/// Where CEF wants the popup widget drawn, relative to the view's top-left.
///
/// In **physical** pixels, like everything else a host hands to or takes from
/// this API. CEF reports popup geometry in DIP; welding scales it up so the
/// rect can be used directly as a viewport.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PopupRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// A popup widget surface: `<select>` dropdowns, autocomplete lists, date
/// pickers.
///
/// Under windowless rendering CEF paints these as a **separate** OSR element
/// rather than compositing them into the view, so a host that only ever draws
/// [`CefSurfaceProducer::acquire_frame`] shows a page whose dropdowns silently
/// do nothing. Draw this over the view at [`PopupRect`] while
/// [`CefSurfaceProducer::popup_rect`] keeps returning `Some`.
///
/// Note this is unrelated to [`NavigationEvent::NewWindowRequested`], which is
/// a request for a whole new *browser* (`window.open`, `target="_blank"`).
pub struct PopupSurface {
    pub texture: ImportedTexture,
    pub rect: PopupRect,
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
    /// Display scale factor, e.g. `2.0` for a 2x HiDPI screen.
    ///
    /// `initial_size` and every other size and coordinate in this API stay in
    /// **physical** pixels; this tells CEF how many of those go to one CSS
    /// pixel. Leave it at 1.0 and a HiDPI host gets a page laid out at twice
    /// the CSS width, which renders everything at half its intended size.
    ///
    /// Follow the window at runtime with
    /// [`CefSurfaceProducer::set_scale_factor`].
    pub scale_factor: f32,
}

impl Default for CefSurfaceConfig {
    fn default() -> Self {
        CefSurfaceConfig {
            initial_url: "about:blank".into(),
            initial_size: PhysicalSize::new(800, 600),
            transparent: false,
            prefer_accelerated: true,
            user_data_dir: None,
            scale_factor: 1.0,
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

    fn capabilities(&self) -> CefSurfaceCapabilities {
        CefSurfaceCapabilities::probe()
    }

    /// Acquire the most recently painted frame as a wgpu texture.
    /// Returns `Ok(None)` if no new frame is available since the last call.
    fn acquire_frame(
        &mut self,
        ctx: &HostWgpuContext,
    ) -> Result<Option<ImportedTexture>, WeldError>;

    /// Acquire the most recently painted **popup widget** surface, if one is
    /// showing and has repainted since the last call.
    ///
    /// Hosts should cache the returned surface and keep drawing it over the
    /// view for as long as [`popup_rect`](Self::popup_rect) returns `Some`;
    /// like [`acquire_frame`](Self::acquire_frame), this only yields on a new
    /// paint. Drop the cached surface when `popup_rect` goes to `None`.
    fn acquire_popup(
        &mut self,
        _ctx: &HostWgpuContext,
    ) -> Result<Option<PopupSurface>, WeldError> {
        Ok(None)
    }

    /// Where the popup widget is showing, or `None` when no popup is open.
    ///
    /// This is the visibility signal: CEF hides a popup without painting
    /// anything, so a host that waits for a frame would leave a stale dropdown
    /// on screen.
    fn popup_rect(&self) -> Option<PopupRect> {
        None
    }

    /// Resize the view. `size` is in **physical** pixels.
    fn resize(&mut self, size: PhysicalSize<u32>) -> Result<(), WeldError>;

    /// Follow the window's display scale, e.g. after it moves between a 1x and
    /// a 2x screen. Live rather than construction-time, because a window can
    /// cross displays without ever being recreated.
    fn set_scale_factor(&mut self, _scale: f32) -> Result<(), WeldError> {
        Err(WeldError::PlatformUnsupported("scale factor is not wired for this producer"))
    }

    /// The scale factor currently reported to CEF.
    fn scale_factor(&self) -> f32 {
        1.0
    }

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

    fn execute_script_with_result(
        &mut self,
        script: &str,
        timeout: std::time::Duration,
    ) -> Result<String, WeldError> {
        let _ = (script, timeout);
        Err(WeldError::BrowserOp(
            "CEF result-bearing script bridge is not wired yet".into(),
        ))
    }

    fn set_cookie(&mut self, cookie: &Cookie) -> Result<(), WeldError> {
        let _ = cookie;
        Err(WeldError::BrowserOp(
            "CEF cookie manager is not wired yet".into(),
        ))
    }

    fn get_cookies_for_url(&mut self, url: &str) -> Result<Vec<Cookie>, WeldError> {
        let _ = url;
        Err(WeldError::BrowserOp(
            "CEF cookie reads are not wired yet".into(),
        ))
    }

    fn delete_cookie(&mut self, cookie: &Cookie) -> Result<(), WeldError> {
        let _ = cookie;
        Err(WeldError::BrowserOp(
            "CEF cookie delete is not wired yet".into(),
        ))
    }

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
