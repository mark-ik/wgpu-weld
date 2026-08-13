use dpi::PhysicalSize;

use crate::{
    auth::AuthId,
    downloads::DownloadId,
    permissions::{PermissionId, PermissionKind},
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
    pub auth_challenges: BrowserFeatureStatus,
    pub permission_requests: BrowserFeatureStatus,
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
            cookies: BrowserFeatureStatus::Supported,
            cookie_change_events: BrowserFeatureStatus::Unsupported(
                "CEF cookie observers are not wired yet",
            ),
            script_execution: BrowserFeatureStatus::Supported,
            script_result: BrowserFeatureStatus::Supported,
            // Windows opens a real DevTools window; Linux takes the call
            // without crashing but has not been seen to open one; macOS
            // crashes CEF outright, so the producer refuses there.
            #[cfg(not(target_os = "macos"))]
            devtools: BrowserFeatureStatus::Supported,
            #[cfg(target_os = "macos")]
            devtools: BrowserFeatureStatus::Unsupported(
                "DevTools crashes CEF 148 for windowless browsers on macOS",
            ),
            // A CefDownloadHandler is registered on every producer. Whether a
            // download can actually land is a per-producer question --
            // `download_dir` decides that -- so this reports the wiring, and
            // `cancel_download` says so plainly when the directory is unset.
            downloads: BrowserFeatureStatus::Supported,
            // The handler is registered and the answer path is implemented,
            // but CEF has never been seen to call it. A probe inside
            // GetAuthCredentials counted zero invocations against a top-level
            // 401 that Chromium itself failed with ERR_INVALID_AUTH_CREDENTIALS,
            // with and without CEF_RUNTIME_STYLE_ALLOY, while other methods on
            // that same handler fire normally. Proxy authentication is
            // untested.
            // Both CEF permission callbacks are registered and answerable;
            // a page's own geolocation success callback has been seen to fire
            // after grant_permission on Windows.
            permission_requests: BrowserFeatureStatus::Supported,
            auth_challenges: BrowserFeatureStatus::Partial(
                "GetAuthCredentials is wired and answerable, but CEF 147 was not                  observed to call it for server auth; proxy auth untested",
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
        //
        // `devtools` was the one claim here that was not true when this test
        // was written: `open_devtools` returned a pending-wiring error on all
        // three producers, and this assertion pinned the lie in place rather
        // than catching it. It calls `show_dev_tools` now, checked by opening
        // a real DevTools window on Windows, 2026-08-12.
        assert_eq!(caps.script_execution, BrowserFeatureStatus::Supported);
        #[cfg(not(target_os = "macos"))]
        assert_eq!(caps.devtools, BrowserFeatureStatus::Supported);
        #[cfg(target_os = "macos")]
        assert!(matches!(caps.devtools, BrowserFeatureStatus::Unsupported(_)));
        assert_eq!(caps.console_messages, BrowserFeatureStatus::Supported);
        // Cookies went from a trait default that errored to real producer
        // implementations over CEF's global cookie manager.
        assert_eq!(caps.cookies, BrowserFeatureStatus::Supported);
        // Script results went from impossible to wired when welding gained a
        // CefApp with a render-process handler: evaluation happens in the
        // renderer, so nothing in the browser process could ever answer it.
        assert_eq!(caps.script_result, BrowserFeatureStatus::Supported);
        // A CefDownloadHandler is registered on every producer as of W7a, with
        // the destination decided by `CefSurfaceConfig::download_dir`.
        assert_eq!(caps.downloads, BrowserFeatureStatus::Supported);
        assert!(caps.accelerated_paint_available);
        assert_eq!(caps.preferred_mode, CefSurfaceMode::AcceleratedPaint);

        // Not wired. These carry a reason string rather than a bare status so
        // a host can report *why* to its own user.
        for status in [caps.cookie_change_events, caps.context_menus] {
            assert!(
                matches!(status, BrowserFeatureStatus::Unsupported(reason) if !reason.is_empty()),
                "expected an explained Unsupported, got {status:?}"
            );
        }

        assert_eq!(caps.permission_requests, BrowserFeatureStatus::Supported);

        // Auth is the other split case: implemented and registered, never
        // seen to fire.
        assert!(matches!(caps.auth_challenges, BrowserFeatureStatus::Partial(_)));

        // Popups are the split case: creation is handled, rendering is not.
        assert!(matches!(caps.popups, BrowserFeatureStatus::Partial(_)));
    }

    /// CEF's `cef_color_t` is ARGB, and windowless transparency is enabled by
    /// alpha 0 — there is no partial option. Pin the mapping: `Some` is fully
    /// opaque, `None` is all-zero, and the default is opaque white (an unset
    /// background used to silently render every CSS-background-less page
    /// transparent).
    #[test]
    fn background_color_maps_to_cef_argb() {
        let mut config = CefSurfaceConfig::default();
        assert_eq!(config.background_color, Some([255, 255, 255]));
        assert_eq!(config.cef_background_color(), 0xFFFF_FFFF);

        config.background_color = Some([0x12, 0x34, 0x56]);
        assert_eq!(config.cef_background_color(), 0xFF12_3456);

        config.background_color = None;
        assert_eq!(config.cef_background_color(), 0);
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

// ── Cursor and IME ───────────────────────────────────────────────────────────

/// A cursor shape the page is asking the host to show.
///
/// Vocabulary shared with `scrying`'s `WebSurfaceProducer`, deliberately: a
/// host that abstracts over both lanes should not need two mappings. Note the
/// crossed names against CEF's own: `Pointer` here is the CSS `pointer`, the
/// link hand, which CEF calls `HAND`; CEF's `POINTER` is the plain arrow and
/// maps to `Default`.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum CursorShape {
    Default,
    Pointer,
    Text,
    Wait,
    Crosshair,
    Move,
    NotAllowed,
    Help,
    Progress,
    ResizeNs,
    ResizeEw,
    ResizeNeSw,
    ResizeNwSe,
    ResizeAll,
    Grab,
    Grabbing,
    ZoomIn,
    ZoomOut,
    /// Anything the shared vocabulary has no name for. `"none"` means CEF
    /// asked for a hidden cursor, which the vocabulary cannot express.
    Custom(String),
}

/// Where the text currently being composed sits, so a host can place its IME
/// candidate window under it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImeComposition {
    /// Union of the per-character bounds CEF reported, in **physical** pixels.
    pub bounds: PopupRect,
    /// Selected range within the composition, in characters.
    pub selection_start: u32,
    pub selection_end: u32,
}

/// Configuration for a single CEF browser surface.
pub struct CefSurfaceConfig {
    pub initial_url: String,
    pub initial_size: PhysicalSize<u32>,
    /// What CEF paints where the page itself paints nothing (no CSS
    /// `background` on `<body>`/`<html>`, or during load).
    ///
    /// `Some([r, g, b])` paints those regions that opaque colour. `None`
    /// renders them transparent: the imported texture carries premultiplied
    /// alpha 0 there, which a readback sees as `[0, 0, 0, 0]`. CEF has no
    /// partially-transparent option — alpha is either 0 or 255.
    ///
    /// The default is opaque white, what a normal browser window shows.
    /// Before 0.5.0 this knob did not exist (a `transparent: bool` was
    /// declared but wired to nothing) and every page without its own CSS
    /// background silently rendered transparent.
    pub background_color: Option<[u8; 3]>,
    /// Prefer `OnAcceleratedPaint` over `OnPaint`. If accelerated paint is
    /// unavailable and `cpu-paint-fallback` is enabled, falls back automatically.
    pub prefer_accelerated: bool,
    /// Persistent user-data directory for cookies, storage, etc.
    /// `None` = in-memory / incognito.
    pub user_data_dir: Option<std::path::PathBuf>,
    /// Whether the host answers permission requests itself.
    ///
    /// Off by default, and for the same reason as `handle_auth_challenges`: an
    /// unanswered request leaves the page waiting forever. Left off, `welding`
    /// reports the request and immediately denies it, which is what a browser
    /// does when a user dismisses the prompt.
    pub handle_permission_requests: bool,
    /// Whether the host answers HTTP auth challenges itself.
    ///
    /// Off by default, and deliberately: CEF's auth callback can be answered
    /// later, but an unanswered one holds its request open forever. A host
    /// that has not wired [`NavigationEvent::AuthChallenged`] up would hang
    /// every authenticated request rather than fail it. Left off, `welding`
    /// reports the challenge and immediately declines it, which the page sees
    /// as an ordinary authentication failure.
    pub handle_auth_challenges: bool,
    /// Where downloads are written. `None`, the default, refuses them.
    ///
    /// This is policy rather than a per-download question because CEF asks
    /// where to put the file inside a callback it will cancel the download
    /// without an answer to, and on Linux and macOS the thread it asks on is
    /// the one that would have to carry the host's reply. The host still
    /// steers each transfer afterwards with
    /// [`CefSurfaceProducer::cancel_download`] and friends.
    ///
    /// The server's suggested filename is reduced to its final component, so
    /// it cannot place a file outside this directory.
    pub download_dir: Option<std::path::PathBuf>,
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
            background_color: Some([255, 255, 255]),
            prefer_accelerated: true,
            user_data_dir: None,
            handle_permission_requests: false,
            handle_auth_challenges: false,
            download_dir: None,
            scale_factor: 1.0,
        }
    }
}

impl CefSurfaceConfig {
    /// [`Self::background_color`] as CEF's ARGB `cef_color_t`: opaque for
    /// `Some`, all-zero (which is what enables transparent windowless
    /// painting) for `None`.
    ///
    /// Only the producers consume this and they only exist under the feature,
    /// but the unit tests below cover it in every configuration, so this is an
    /// allow rather than a cfg gate.
    #[cfg_attr(not(feature = "cef-runtime"), allow(dead_code))]
    pub(crate) fn cef_background_color(&self) -> u32 {
        match self.background_color {
            Some([r, g, b]) => {
                0xFF00_0000 | (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b)
            }
            None => 0,
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
    /// Tell CEF whether the surface is visible.
    ///
    /// A windowless browser that CEF believes is hidden throttles or drops
    /// work, so a host that never says otherwise can end up with a browser
    /// that paints but ignores input.
    fn set_visible(&mut self, _visible: bool) -> Result<(), WeldError> {
        Err(WeldError::PlatformUnsupported("visibility is not wired for this producer"))
    }

    /// The cursor shape the page is asking for, if it changed since the last
    /// call. The host owns the pointer under windowless rendering, so nothing
    /// happens unless the host applies it.
    fn poll_cursor_shape(&mut self) -> Option<CursorShape> {
        None
    }

    /// Where the text being composed sits, if it moved since the last call.
    /// Use it to place an IME candidate window.
    fn poll_ime_composition(&mut self) -> Option<ImeComposition> {
        None
    }

    /// Set or update the in-progress IME composition (the "preedit" text).
    ///
    /// `selection` is a character range within `text`. CEF draws the
    /// composition itself as part of the page.
    fn ime_set_composition(
        &mut self,
        _text: &str,
        _selection: (u32, u32),
    ) -> Result<(), WeldError> {
        Err(WeldError::PlatformUnsupported("IME is not wired for this producer"))
    }

    /// Commit the composition, or insert `text` directly when there is none.
    fn ime_commit_text(&mut self, _text: &str) -> Result<(), WeldError> {
        Err(WeldError::PlatformUnsupported("IME is not wired for this producer"))
    }

    /// Finish composing and keep what is there.
    fn ime_finish_composing(&mut self, _keep_selection: bool) -> Result<(), WeldError> {
        Err(WeldError::PlatformUnsupported("IME is not wired for this producer"))
    }

    /// Abandon the composition.
    fn ime_cancel_composition(&mut self) -> Result<(), WeldError> {
        Err(WeldError::PlatformUnsupported("IME is not wired for this producer"))
    }

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

    /// Grant a permission request. Media requests are granted exactly what
    /// they asked for; anything else is accepted.
    fn grant_permission(&mut self, _id: PermissionId) -> Result<(), WeldError> {
        Err(WeldError::PlatformUnsupported(
            "permission requests are not wired for this producer",
        ))
    }

    /// Deny a permission request.
    fn deny_permission(&mut self, _id: PermissionId) -> Result<(), WeldError> {
        Err(WeldError::PlatformUnsupported(
            "permission requests are not wired for this producer",
        ))
    }

    /// Answer an auth challenge.
    ///
    /// Neither the username nor the password is logged or kept; both go
    /// straight to CEF. Answering an id twice, or one that was already
    /// declined, is an error rather than a silent no-op.
    fn answer_auth(
        &mut self,
        _id: AuthId,
        _username: &str,
        _password: &str,
    ) -> Result<(), WeldError> {
        Err(WeldError::PlatformUnsupported(
            "auth challenges are not wired for this producer",
        ))
    }

    /// Decline an auth challenge. The request fails as if the user cancelled.
    fn cancel_auth(&mut self, _id: AuthId) -> Result<(), WeldError> {
        Err(WeldError::PlatformUnsupported(
            "auth challenges are not wired for this producer",
        ))
    }

    /// Ask CEF to cancel, pause or resume a download.
    ///
    /// The request is recorded and applied on that download's next update,
    /// because CEF's download callback is callback-scoped like the paint
    /// handles and does not exist between updates. Updates arrive promptly
    /// while a transfer is running; a request for a download that has already
    /// finished is simply never applied.
    fn cancel_download(&mut self, _id: DownloadId) -> Result<(), WeldError> {
        Err(WeldError::PlatformUnsupported(
            "downloads are not wired for this producer",
        ))
    }

    fn pause_download(&mut self, _id: DownloadId) -> Result<(), WeldError> {
        Err(WeldError::PlatformUnsupported(
            "downloads are not wired for this producer",
        ))
    }

    fn resume_download(&mut self, _id: DownloadId) -> Result<(), WeldError> {
        Err(WeldError::PlatformUnsupported(
            "downloads are not wired for this producer",
        ))
    }

    /// Ask CEF to repaint the whole view now.
    ///
    /// Painting is change-driven, so a view nothing has changed does not paint
    /// on its own. That matters after a render process dies: the host can
    /// navigate again and the page will load, but the replacement renderer has
    /// nothing queued for the surface, so the host keeps presenting its last
    /// pre-crash frame. This is the nudge that gets a first frame out of it.
    fn request_repaint(&mut self) -> Result<(), WeldError> {
        Err(WeldError::PlatformUnsupported(
            "repaint is not wired for this producer",
        ))
    }
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

    /// Evaluate `script` and ask for its value back. Returns the request id.
    ///
    /// The answer arrives through
    /// [`poll_script_result`](Self::poll_script_result), because the value has
    /// to be fetched from the renderer process over CEF's process-message
    /// channel. A blocking call would wait on the very loop that carries the
    /// reply.
    ///
    /// Results come back as JSON: `2+2` yields `4`, `document.title` yields a
    /// quoted string, an object yields an object. A script that throws yields
    /// `Err` with the exception message.
    fn request_script_result(&mut self, _script: &str) -> Result<u32, WeldError> {
        Err(WeldError::BrowserOp("script results are not wired for this producer".into()))
    }

    /// The next finished [`request_script_result`](Self::request_script_result),
    /// in the order the renderer answered.
    fn poll_script_result(&mut self) -> Option<crate::app::ScriptResult> {
        None
    }

    /// Write a cookie, as if `url` had sent it in a `Set-Cookie` header.
    ///
    /// CEF validates the cookie's domain and path against `url` and rejects
    /// mismatches, so pass the URL the cookie belongs to rather than any URL.
    fn set_cookie(&mut self, _url: &str, _cookie: &Cookie) -> Result<(), WeldError> {
        Err(WeldError::BrowserOp("cookies are not wired for this producer".into()))
    }

    /// Start reading cookies. `url` of `None` reads the whole store.
    ///
    /// The answer arrives later through [`poll_cookies`](Self::poll_cookies);
    /// CEF delivers cookies through a visitor, one at a time, and on Linux and
    /// macOS the calling thread is CEF's own UI thread, so a blocking getter
    /// would wait on the loop that produces the answer.
    fn request_cookies(&mut self, _url: Option<&str>) -> Result<(), WeldError> {
        Err(WeldError::BrowserOp("cookies are not wired for this producer".into()))
    }

    /// The result of a [`request_cookies`](Self::request_cookies), once the
    /// read has finished. An empty `Vec` means the store had none, which is a
    /// different answer from `None`.
    fn poll_cookies(&mut self) -> Option<Vec<Cookie>> {
        None
    }

    /// Delete cookies. `url` of `None` means every URL, `name` of `None` means
    /// every name, so both `None` clears the store.
    fn delete_cookies(&mut self, _url: Option<&str>, _name: Option<&str>) -> Result<(), WeldError> {
        Err(WeldError::BrowserOp("cookies are not wired for this producer".into()))
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

/// Map CEF's termination status onto the platform-neutral one.
///
/// Compared against the associated constants rather than matched: the status
/// is a newtype over a C enum whose repr differs by platform, so `==` is the
/// portable read. An unrecognised value is carried through rather than
/// flattened into `Abnormal`, so a newer CEF cannot silently look ordinary.
#[cfg(feature = "cef-runtime")]
pub(crate) fn termination_status(status: cef::TerminationStatus) -> ProcessTerminationStatus {
    use cef::TerminationStatus as Ts;
    if status == Ts::PROCESS_WAS_KILLED {
        ProcessTerminationStatus::Killed
    } else if status == Ts::PROCESS_CRASHED {
        ProcessTerminationStatus::Crashed
    } else if status == Ts::PROCESS_OOM {
        ProcessTerminationStatus::OutOfMemory
    } else if status == Ts::LAUNCH_FAILED {
        ProcessTerminationStatus::LaunchFailed
    } else if status == Ts::INTEGRITY_FAILURE {
        ProcessTerminationStatus::IntegrityFailure
    } else if status == Ts::ABNORMAL_TERMINATION {
        ProcessTerminationStatus::Abnormal
    } else {
        ProcessTerminationStatus::Unknown(*status.as_ref() as i32)
    }
}

/// Why a render process died, as CEF reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessTerminationStatus {
    /// Ended abnormally, with no more specific reason.
    Abnormal,
    /// Killed from outside, e.g. by a task manager or a signal.
    Killed,
    /// Crashed.
    Crashed,
    /// Ran out of memory. Reloading on a loop will only repeat this.
    OutOfMemory,
    /// The process never started.
    LaunchFailed,
    /// Failed an integrity check.
    IntegrityFailure,
    /// A status this build of `welding` does not recognise; the raw value is
    /// carried so a host can still log something useful.
    Unknown(i32),
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
    /// The render process backing this browser died. The browser object
    /// itself survives, and `reload()` brings the page back: Chromium spawns a
    /// fresh renderer for the next navigation. `status` says which kind of
    /// death it was, which is what decides whether reloading is sensible --
    /// retrying an `OutOfMemory` in a loop just kills the machine slower.
    ContentProcessTerminated {
        status: ProcessTerminationStatus,
        /// CEF's error code, 0 when it supplies none.
        error_code: i32,
        /// CEF's description, empty when it supplies none.
        error_string: String,
    },
    NewWindowRequested {
        url: String,
        user_gesture: bool,
    },
    /// A page asked for something Chromium gates behind a prompt: the
    /// camera, the microphone, the user's location, notifications.
    ///
    /// Answer with [`CefSurfaceProducer::grant_permission`] or
    /// [`CefSurfaceProducer::deny_permission`], quoting `id`. If
    /// `CefSurfaceConfig::handle_permission_requests` is off, `welding` has
    /// already denied it and this is a notification rather than a question.
    PermissionRequested {
        id: PermissionId,
        /// The origin asking, e.g. `https://example.com/`.
        origin: String,
        /// What was asked for. Bits this build does not name still arrive, as
        /// `PermissionKind::Other`.
        permissions: Vec<PermissionKind>,
        /// CEF's raw bitmask, for hosts that want to be exact.
        raw: u32,
    },
    /// A server or proxy demanded credentials.
    ///
    /// Carries no credentials, only the challenge. Answer with
    /// [`CefSurfaceProducer::answer_auth`] or decline with
    /// [`CefSurfaceProducer::cancel_auth`], quoting `id`. If
    /// `CefSurfaceConfig::handle_auth_challenges` is left off, `welding` has
    /// already declined it by the time this arrives and the id is spent — the
    /// event is then a notification, not a question.
    ///
    /// CEF has one challenge channel and reports whether it came from a proxy;
    /// it does not say whether a page load or a download provoked it, so
    /// unlike `scrying` there is no page/download split to report.
    AuthChallenged {
        id: AuthId,
        /// The URL whose load triggered the challenge.
        origin_url: String,
        /// Host the credentials are for.
        host: String,
        port: u16,
        realm: String,
        /// e.g. `basic`, `digest`, `negotiate`.
        scheme: String,
        /// True when a proxy is asking rather than the origin server.
        is_proxy: bool,
    },
    /// A download began. `welding` has already chosen `destination_path`
    /// under the configured download directory and accepted the transfer; the
    /// host owns any UI. The `id` ties the later events to this one.
    DownloadStarted {
        id: DownloadId,
        url: String,
        suggested_filename: String,
        destination_path: std::path::PathBuf,
        /// What the server announced, when it announced anything.
        total_bytes_expected: Option<u64>,
    },
    /// Throttled progress, at most ten a second per download, plus a final one
    /// when it finishes. `bytes_received` is cumulative.
    DownloadProgress {
        id: DownloadId,
        bytes_received: u64,
        total_bytes_expected: Option<u64>,
    },
    /// A download ended. `error` is `Some` when it failed, in which case the
    /// file may be partial or absent. Host-driven cancellation arrives as
    /// `DownloadCancelled` instead.
    DownloadFinished {
        id: DownloadId,
        destination_path: std::path::PathBuf,
        error: Option<String>,
    },
    /// A download was cancelled, either by `cancel_download` or by CEF.
    ///
    /// There is no resume blob: CEF exposes live pause and resume on a running
    /// download but nothing that survives the process, so a cancelled download
    /// starts over.
    DownloadCancelled {
        id: DownloadId,
        destination_path: std::path::PathBuf,
    },
    ConsoleMessage {
        level: i32,
        message: String,
        source: String,
        line: i32,
    },
}
