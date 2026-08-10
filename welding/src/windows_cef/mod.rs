/// Windows CEF producer: accelerated OSR via `OnAcceleratedPaint`.
///
/// # Handle lifetime
///
/// `CefAcceleratedPaintInfo::shared_texture_handle` is callback-scoped. Under
/// `cef-runtime` the `on_accelerated_paint` callback calls `DuplicateHandle`,
/// opens that duplicate on the host device, and copies into an application-owned
/// texture before returning. Only that owned texture crosses the callback
/// boundary.
///
/// # Threading
///
/// CEF invokes `on_accelerated_paint` on the render thread. The host calls
/// `acquire_frame` and browser-control methods from the winit/wgpu thread.
/// The copied-frame and browser slots are protected by `Mutex`. On Windows,
/// CEF owns its dedicated UI thread and proxies browser-control operations
/// called from the host thread.
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicI32, Ordering},
        Arc, Mutex,
    },
};
#[cfg(feature = "cef-runtime")]
use std::sync::atomic::{AtomicBool, AtomicU64};

use dpi::PhysicalSize;

use crate::{
    error::WeldError,
    native_frame::{HostWgpuContext, ImportedTexture},
    runtime::CefRuntime,
    surface::{
        CefSurfaceConfig, CefSurfaceMode, CefSurfaceProducer, FocusDirection, KeyEvent, MouseEvent,
        NavigationEvent,
    },
};
#[cfg(feature = "cef-runtime")]
use crate::native_frame::{D3d11CallbackFrameCopier, Dx12SharedTexture, WgpuTextureImporter};

#[cfg(feature = "cef-runtime")]
use cef::{ImplBrowser, ImplBrowserHost, ImplFrame};

// ── Public config ─────────────────────────────────────────────────────────────

pub struct WindowsCefConfig {
    pub surface: CefSurfaceConfig,
}
impl Default for WindowsCefConfig {
    fn default() -> Self {
        Self { surface: CefSurfaceConfig::default() }
    }
}

// ── Shared callback state ─────────────────────────────────────────────────────

struct EventQueues {
    nav: VecDeque<NavigationEvent>,
    web_messages: VecDeque<String>,
}

// Under cef-runtime the render handler and the producer share this Arc so that
// resize() can update the size the render handler reports in view_rect().
#[cfg(feature = "cef-runtime")]
#[derive(Clone)]
struct WeldRenderHandlerInner {
    frame_slot: Arc<Mutex<Option<ImportedTexture>>>,
    next_generation: Arc<AtomicU64>,
    host_ctx: HostWgpuContext,
    callback_copier: Arc<D3d11CallbackFrameCopier>,
    events: Arc<Mutex<EventQueues>>,
    size: Arc<Mutex<PhysicalSize<u32>>>,
}

#[cfg(feature = "cef-runtime")]
#[derive(Clone)]
struct WeldLifeSpanState {
    closed: Arc<AtomicBool>,
    close_requested: Arc<AtomicBool>,
    browser_id: Arc<AtomicI32>,
    browser: Arc<Mutex<Option<cef::Browser>>>,
}

// ── cef-runtime: render handler + client ─────────────────────────────────────

#[cfg(feature = "cef-runtime")]
mod cef_backed;

// ── Producer struct ───────────────────────────────────────────────────────────

pub struct WindowsCefProducer {
    browser_id: Arc<AtomicI32>,
    #[cfg(feature = "cef-runtime")]
    browser: Arc<Mutex<Option<cef::Browser>>>,
    #[cfg(feature = "cef-runtime")]
    cef_size: Arc<Mutex<PhysicalSize<u32>>>,
    #[cfg(feature = "cef-runtime")]
    closed: Arc<AtomicBool>,
    #[cfg(feature = "cef-runtime")]
    close_requested: Arc<AtomicBool>,
    frame_slot: Arc<Mutex<Option<ImportedTexture>>>,
    events: Arc<Mutex<EventQueues>>,
    size: PhysicalSize<u32>,
}

// Safety: CefSurfaceProducer is Send; cef::Browser wraps *mut CEF objects whose
// ref-counts are thread-safe. On Windows, CEF proxies browser-host operations
// from the host thread to its dedicated UI thread.
#[cfg(feature = "cef-runtime")]
unsafe impl Send for WindowsCefProducer {}

impl WindowsCefProducer {
    /// Create a CEF browser in OSR (windowless + shared-texture) mode.
    ///
    /// Under `cef-runtime`: wires the CEF `Client` + `RenderHandler` vtables,
    /// starts async browser creation, and returns a producer whose browser slot
    /// is populated by `on_after_created`. Requires [`CefRuntime::initialize`]
    /// to have been called.
    ///
    /// Without `cef-runtime`: returns `Err(SurfaceCreation(...))` — vtable
    /// wiring is pending `cef-runtime` enablement.
    pub fn new(
        _runtime: &CefRuntime,
        config: WindowsCefConfig,
        host_ctx: &HostWgpuContext,
    ) -> Result<Self, WeldError> {
        #[cfg(feature = "cef-runtime")]
        {
            let initial_size = config.surface.initial_size;
            let frame_slot = Arc::new(Mutex::new(None));
            let next_generation = Arc::new(AtomicU64::new(0));
            let events =
                Arc::new(Mutex::new(EventQueues { nav: VecDeque::new(), web_messages: VecDeque::new() }));
            let cef_size = Arc::new(Mutex::new(initial_size));
            let closed = Arc::new(AtomicBool::new(false));
            let close_requested = Arc::new(AtomicBool::new(false));
            let browser_id = Arc::new(AtomicI32::new(0));
            let browser = Arc::new(Mutex::new(None));
            let callback_copier = Arc::new(D3d11CallbackFrameCopier::new(host_ctx)?);

            let inner = WeldRenderHandlerInner {
                frame_slot: frame_slot.clone(),
                next_generation,
                host_ctx: host_ctx.clone(),
                callback_copier,
                events: events.clone(),
                size: cef_size.clone(),
            };
            let life_span_state = WeldLifeSpanState {
                closed: closed.clone(),
                close_requested: close_requested.clone(),
                browser_id: browser_id.clone(),
                browser: browser.clone(),
            };

            let render_handler = cef_backed::WeldRenderHandler::build(inner.clone());
            let life_span_handler =
                cef_backed::WeldLifeSpanHandler::build(life_span_state, events.clone());
            let load_handler = cef_backed::WeldLoadHandler::build(inner.clone());
            let display_handler = cef_backed::WeldDisplayHandler::build(inner);
            let request_handler = cef_backed::WeldRequestHandler::build(events.clone());
            let mut client = cef_backed::WeldClient::build(
                render_handler,
                life_span_handler,
                load_handler,
                display_handler,
                request_handler,
                events.clone(),
            );

            let window_info = cef::WindowInfo {
                windowless_rendering_enabled: 1,
                shared_texture_enabled: 1,
                // CEF self-drives paints at `windowless_frame_rate`. Setting
                // this to 1 requires the host to call SendExternalBeginFrame.
                external_begin_frame_enabled: 0,
                ..Default::default()
            };
            let browser_settings = cef::BrowserSettings {
                windowless_frame_rate: 60,
                ..Default::default()
            };
            let url: cef::CefString = config.surface.initial_url.as_str().into();

            let create_started = cef::browser_host_create_browser(
                Some(&window_info),
                Some(&mut client),
                Some(&url),
                Some(&browser_settings),
                None,
                None,
            );
            if create_started == 0 {
                return Err(WeldError::SurfaceCreation(
                    "browser_host_create_browser returned false".into(),
                ));
            }

            return Ok(WindowsCefProducer {
                browser_id,
                browser,
                cef_size,
                closed,
                close_requested,
                frame_slot,
                events,
                size: initial_size,
            });
        }

        #[cfg(not(feature = "cef-runtime"))]
        {
            let _ = (_runtime, config, host_ctx);
            Err(WeldError::SurfaceCreation(
                "Windows CEF vtable wiring requires the `cef-runtime` feature".into(),
            ))
        }
    }

    pub fn is_closed(&self) -> bool {
        #[cfg(feature = "cef-runtime")]
        {
            self.closed.load(Ordering::Acquire)
        }
        #[cfg(not(feature = "cef-runtime"))]
        {
            self.browser_id.load(Ordering::Acquire) == 0
        }
    }

    #[cfg(feature = "cef-runtime")]
    fn browser(&self) -> Option<cef::Browser> {
        self.browser.lock().unwrap().clone()
    }
}

// ── CefSurfaceProducer impl ───────────────────────────────────────────────────

// unreachable_code: cfg-gated fallback Errs are unreachable on the cef-runtime path.
// unused_variables/mut: parameters used only in cfg(cef-runtime) branches appear unused on scaffold path.
#[allow(unreachable_code, unused_mut, unused_variables)]
impl CefSurfaceProducer for WindowsCefProducer {
    fn surface_mode(&self) -> CefSurfaceMode {
        CefSurfaceMode::AcceleratedPaint
    }

    fn acquire_frame(
        &mut self,
        _ctx: &HostWgpuContext,
    ) -> Result<Option<ImportedTexture>, WeldError> {
        Ok(self.frame_slot.lock().unwrap().take())
    }

    fn resize(&mut self, size: PhysicalSize<u32>) -> Result<(), WeldError> {
        self.size = size;
        #[cfg(feature = "cef-runtime")]
        {
            *self.cef_size.lock().unwrap() = size;
            if let Some(mut host) = self.browser().and_then(|browser| browser.host()) {
                host.was_resized();
                // Force a fresh paint for newly exposed regions after resize.
                host.invalidate(cef::PaintElementType::default());
            }
            return Ok(());
        }
        Err(pending("cef_browser_host_t::was_resized"))
    }

    fn navigate_to_url(&mut self, url: &str) -> Result<(), WeldError> {
        #[cfg(feature = "cef-runtime")]
        if let Some(mut frame) = self.browser().and_then(|browser| browser.main_frame()) {
            frame.load_url(Some(&url.into()));
            return Ok(());
        }
        Err(pending("cef_frame_t::load_url"))
    }

    fn navigate_to_string(&mut self, content: &str, _mime_type: &str) -> Result<(), WeldError> {
        #[cfg(feature = "cef-runtime")]
        if let Some(mut frame) = self.browser().and_then(|browser| browser.main_frame()) {
            frame.load_url(Some(&content.into()));
            return Ok(());
        }
        Err(pending("cef_frame_t::load_string"))
    }

    fn reload(&mut self) -> Result<(), WeldError> {
        #[cfg(feature = "cef-runtime")]
        {
            if let Some(browser) = self.browser() {
                browser.reload();
                return Ok(());
            }
        }
        Err(pending("cef_browser_t::reload"))
    }

    fn stop(&mut self) -> Result<(), WeldError> {
        #[cfg(feature = "cef-runtime")]
        {
            if let Some(browser) = self.browser() {
                browser.stop_load();
                return Ok(());
            }
        }
        Err(pending("cef_browser_t::stop_load"))
    }

    fn go_back(&mut self) -> Result<(), WeldError> {
        #[cfg(feature = "cef-runtime")]
        {
            if let Some(browser) = self.browser() {
                browser.go_back();
                return Ok(());
            }
        }
        Err(pending("cef_browser_t::go_back"))
    }

    fn go_forward(&mut self) -> Result<(), WeldError> {
        #[cfg(feature = "cef-runtime")]
        {
            if let Some(browser) = self.browser() {
                browser.go_forward();
                return Ok(());
            }
        }
        Err(pending("cef_browser_t::go_forward"))
    }

    fn send_mouse_input(&mut self, event: MouseEvent) -> Result<(), WeldError> {
        #[cfg(feature = "cef-runtime")]
        if let Some(host) = self.browser().and_then(|browser| browser.host()) {
            crate::cef_input::send_mouse(&host, &event);
            return Ok(());
        }
        Err(pending("cef_browser_host_t mouse input"))
    }

    fn send_keyboard_input(&mut self, event: KeyEvent) -> Result<(), WeldError> {
        #[cfg(feature = "cef-runtime")]
        if let Some(host) = self.browser().and_then(|browser| browser.host()) {
            crate::cef_input::send_key(&host, &event);
            return Ok(());
        }
        Err(pending("cef_browser_host_t::send_key_event"))
    }

    fn move_focus(&mut self, direction: FocusDirection) -> Result<(), WeldError> {
        #[cfg(feature = "cef-runtime")]
        if let Some(host) = self.browser().and_then(|browser| browser.host()) {
            crate::cef_input::set_focus(&host, direction);
            return Ok(());
        }
        Err(pending("cef_browser_host_t::set_focus"))
    }

    fn post_web_message(&mut self, message: &str) -> Result<(), WeldError> {
        // Escape the message as a JS string literal and dispatch a MessageEvent
        // on window so that `window.addEventListener("message", ...)` works.
        let escaped = escape_js_string(message);
        let script = format!(
            "window.dispatchEvent(new MessageEvent('message',{{data:{escaped},origin:'weld'}}));"
        );
        self.execute_script(&script, "weld://post_web_message")
    }

    fn poll_web_message(&mut self) -> Option<String> {
        self.events.lock().unwrap().web_messages.pop_front()
    }

    fn poll_navigation_event(&mut self) -> Option<NavigationEvent> {
        self.events.lock().unwrap().nav.pop_front()
    }

    fn execute_script(&mut self, script: &str, source_url: &str) -> Result<(), WeldError> {
        #[cfg(feature = "cef-runtime")]
        if let Some(frame) = self.browser().and_then(|browser| browser.main_frame()) {
            let code: cef::CefString = script.into();
            let url: cef::CefString = source_url.into();
            frame.execute_java_script(Some(&code), Some(&url), 0);
            return Ok(());
        }
        Err(pending("cef_frame_t::execute_java_script"))
    }

    fn open_devtools(&self) -> Result<(), WeldError> {
        Err(pending("cef_browser_host_t::show_dev_tools"))
    }

    fn browser_id(&self) -> i32 {
        self.browser_id.load(Ordering::Acquire)
    }

    fn close(&mut self) -> Result<(), WeldError> {
        #[cfg(feature = "cef-runtime")]
        {
            self.closed.store(false, Ordering::Release);
            self.close_requested.store(true, Ordering::Release);
            if let Some(mut host) = self.browser().and_then(|browser| browser.host()) {
                eprintln!("weld: requesting CEF browser close");
                host.close_browser(true as _);
            } else {
                eprintln!("weld: browser close queued until CEF on_after_created");
            }
            return Ok(());
        }
        self.browser_id.store(0, Ordering::Release);
        Err(pending("cef_browser_host_t::close_browser"))
    }
}

fn pending(op: &'static str) -> WeldError {
    WeldError::BrowserOp(format!("{op}: requires `cef-runtime` feature or pending wiring"))
}

/// Encode `s` as a JSON string literal (double-quoted, backslash-escapes only).
/// Used to build a JS snippet that dispatches a MessageEvent without eval-injection risk.
fn escape_js_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"'  => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                use std::fmt::Write;
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c    => out.push(c),
        }
    }
    out.push('"');
    out
}
