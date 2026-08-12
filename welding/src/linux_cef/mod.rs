/// Linux CEF producer: accelerated OSR via native pixmap / DMABUF planes
/// and Vulkan external memory.
///
/// # DMABUF fd lifetime
///
/// Each plane fd in `AcceleratedPaintInfo` is callback-scoped. The
/// `on_accelerated_paint` callback calls `dup(2)` on every fd before storing
/// the planes in [`DmaBufImage`](crate::native_frame::DmaBufImage). The Vulkan
/// importer takes ownership of the duped fds on success; otherwise
/// `DmaBufImage::Drop` closes them.
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

use dpi::PhysicalSize;

use crate::{
    error::WeldError,
    native_frame::{HostWgpuContext, ImportedTexture, PendingFrameSlot, WgpuTextureImporter},
    runtime::CefRuntime,
    surface::{
        CefSurfaceConfig, CefSurfaceMode, CefSurfaceProducer, FocusDirection, KeyEvent, MouseEvent,
        NavigationEvent,
    },
};

#[cfg(feature = "cef-runtime")]
use cef::{ImplBrowser, ImplBrowserHost, ImplFrame};

// ── Public config ─────────────────────────────────────────────────────────────

pub struct LinuxCefConfig {
    pub surface: CefSurfaceConfig,
}

impl Default for LinuxCefConfig {
    fn default() -> Self {
        Self { surface: CefSurfaceConfig::default() }
    }
}

// ── Shared callback state ─────────────────────────────────────────────────────

struct EventQueues {
    nav: VecDeque<NavigationEvent>,
    web_messages: VecDeque<String>,
}

#[cfg(feature = "cef-runtime")]
#[derive(Clone)]
struct WeldRenderHandlerInner {
    frame_slot: Arc<Mutex<PendingFrameSlot>>,
    popup_slot: Arc<Mutex<PendingFrameSlot>>,
    popup: Arc<crate::popup::PopupState>,
    cursor: Arc<crate::cursor::LatestCursor>,
    ime: Arc<crate::ime::LatestComposition>,
    events: Arc<Mutex<EventQueues>>,
    metrics: Arc<Mutex<crate::view::ViewMetrics>>,
}

// ── cef-runtime: render handler + client ─────────────────────────────────────

#[cfg(feature = "cef-runtime")]
mod cef_backed;

// ── Producer struct ───────────────────────────────────────────────────────────

pub struct LinuxCefProducer {
    browser_id: i32,
    #[cfg(feature = "cef-runtime")]
    browser: cef::Browser,
    #[cfg(feature = "cef-runtime")]
    metrics: Arc<Mutex<crate::view::ViewMetrics>>,
    frame_slot: Arc<Mutex<PendingFrameSlot>>,
    #[cfg(feature = "cef-runtime")]
    popup_slot: Arc<Mutex<PendingFrameSlot>>,
    #[cfg(feature = "cef-runtime")]
    popup: Arc<crate::popup::PopupState>,
    #[cfg(feature = "cef-runtime")]
    cursor: Arc<crate::cursor::LatestCursor>,
    #[cfg(feature = "cef-runtime")]
    ime: Arc<crate::ime::LatestComposition>,
    events: Arc<Mutex<EventQueues>>,
    size: PhysicalSize<u32>,
}

#[cfg(feature = "cef-runtime")]
unsafe impl Send for LinuxCefProducer {}

impl LinuxCefProducer {
    pub fn new(_runtime: &CefRuntime, config: LinuxCefConfig) -> Result<Self, WeldError> {
        #[cfg(feature = "cef-runtime")]
        {
            let initial_size = config.surface.initial_size;
            let frame_slot = Arc::new(Mutex::new(PendingFrameSlot::default()));
            let events = Arc::new(Mutex::new(EventQueues {
                nav: VecDeque::new(),
                web_messages: VecDeque::new(),
            }));
            let metrics = Arc::new(Mutex::new(crate::view::ViewMetrics::new(
                initial_size,
                config.surface.scale_factor,
            )));

            let popup_slot = Arc::new(Mutex::new(PendingFrameSlot::default()));
            let popup = Arc::new(crate::popup::PopupState::default());
            let cursor = Arc::new(crate::cursor::LatestCursor::default());
            let ime = Arc::new(crate::ime::LatestComposition::default());

            let inner = WeldRenderHandlerInner {
                frame_slot: frame_slot.clone(),
                popup_slot: popup_slot.clone(),
                popup: popup.clone(),
                cursor: cursor.clone(),
                ime: ime.clone(),
                events: events.clone(),
                metrics: metrics.clone(),
            };
            let render_handler = cef_backed::WeldRenderHandler::build(inner.clone());
            let load_handler = cef_backed::WeldLoadHandler::build(inner.clone());
            let display_handler = cef_backed::WeldDisplayHandler::build(inner);
            let life_span_handler = cef_backed::WeldLifeSpanHandler::build(events.clone());
            let request_handler = cef_backed::WeldRequestHandler::build(events.clone());
            let mut client = cef_backed::WeldClient::build(
                render_handler,
                load_handler,
                display_handler,
                life_span_handler,
                request_handler,
                events.clone(),
            );

            let window_info = cef::WindowInfo {
                windowless_rendering_enabled: 1,
                shared_texture_enabled: 1,
                // external_begin_frame_enabled = 0 lets CEF drive paints
                // itself at `windowless_frame_rate`. Setting it to 1 would
                // require the host to call SendExternalBeginFrame on every
                // tick (e.g. to genlock with the host renderer's vsync).
                external_begin_frame_enabled: 0,
                ..Default::default()
            };
            let browser_settings = cef::BrowserSettings {
                windowless_frame_rate: 60,
                ..Default::default()
            };
            let url: cef::CefString = config.surface.initial_url.as_str().into();

            let browser = cef::browser_host_create_browser_sync(
                Some(&window_info),
                Some(&mut client),
                Some(&url),
                Some(&browser_settings),
                None,
                None,
            )
            .ok_or_else(|| {
                WeldError::SurfaceCreation("browser_host_create_browser_sync returned None".into())
            })?;

            let browser_id = browser.identifier();
            return Ok(LinuxCefProducer {
                browser_id,
                browser,
                metrics,
                frame_slot,
                popup_slot,
                popup,
                cursor,
                ime,
                events,
                size: initial_size,
            });
        }

        #[cfg(not(feature = "cef-runtime"))]
        {
            let _ = (_runtime, config);
            Err(WeldError::SurfaceCreation(
                "Linux CEF vtable wiring requires the `cef-runtime` feature".into(),
            ))
        }
    }
}

// ── CefSurfaceProducer impl ───────────────────────────────────────────────────

#[allow(unreachable_code, unused_mut, unused_variables)]
impl CefSurfaceProducer for LinuxCefProducer {
    fn surface_mode(&self) -> CefSurfaceMode {
        CefSurfaceMode::AcceleratedPaint
    }

    fn acquire_frame(
        &mut self,
        ctx: &HostWgpuContext,
    ) -> Result<Option<ImportedTexture>, WeldError> {
        let frame = self.frame_slot.lock().unwrap().take();
        match frame {
            None => Ok(None),
            Some(f) => Ok(Some(WgpuTextureImporter::import(f, ctx)?)),
        }
    }

    fn acquire_popup(
        &mut self,
        ctx: &HostWgpuContext,
    ) -> Result<Option<crate::surface::PopupSurface>, WeldError> {
        #[cfg(feature = "cef-runtime")]
        {
            let Some(rect) = self.popup.rect_if_visible() else {
                return Ok(None);
            };
            // CEF reports popup geometry in DIP; hosts draw in physical pixels.
            let rect = self.metrics.lock().unwrap().rect_to_physical(rect);
            let frame = self.popup_slot.lock().unwrap().take();
            return match frame {
                None => Ok(None),
                Some(f) => Ok(Some(crate::surface::PopupSurface {
                    texture: WgpuTextureImporter::import(f, ctx)?,
                    rect,
                })),
            };
        }
        #[cfg(not(feature = "cef-runtime"))]
        {
            Ok(None)
        }
    }

    fn popup_rect(&self) -> Option<crate::surface::PopupRect> {
        #[cfg(feature = "cef-runtime")]
        {
            let rect = self.popup.rect_if_visible()?;
            return Some(self.metrics.lock().unwrap().rect_to_physical(rect));
        }
        #[cfg(not(feature = "cef-runtime"))]
        {
            None
        }
    }

    fn resize(&mut self, size: PhysicalSize<u32>) -> Result<(), WeldError> {
        self.size = size;
        #[cfg(feature = "cef-runtime")]
        {
            self.metrics.lock().unwrap().set_size(size);
            if let Some(mut host) = self.browser.host() {
                host.was_resized();
            }
            return Ok(());
        }
        Err(pending("cef_browser_host_t::was_resized"))
    }

    fn poll_cursor_shape(&mut self) -> Option<crate::surface::CursorShape> {
        #[cfg(feature = "cef-runtime")]
        {
            return self.cursor.take();
        }
        #[cfg(not(feature = "cef-runtime"))]
        {
            None
        }
    }

    fn poll_ime_composition(&mut self) -> Option<crate::surface::ImeComposition> {
        #[cfg(feature = "cef-runtime")]
        {
            return self.ime.take();
        }
        #[cfg(not(feature = "cef-runtime"))]
        {
            None
        }
    }

    fn ime_set_composition(&mut self, text: &str, selection: (u32, u32)) -> Result<(), WeldError> {
        #[cfg(feature = "cef-runtime")]
        if let Some(host) = self.browser.host() {
            let text: cef::CefString = text.into();
            let selection = cef::Range { from: selection.0, to: selection.1 };
            // No underlines: CEF renders the composition inside the page, and
            // the default styling is what a page author expects.
            host.ime_set_composition(Some(&text), None, None, Some(&selection));
            return Ok(());
        }
        Err(WeldError::PlatformUnsupported("IME requires the cef-runtime feature"))
    }

    fn ime_commit_text(&mut self, text: &str) -> Result<(), WeldError> {
        #[cfg(feature = "cef-runtime")]
        if let Some(host) = self.browser.host() {
            let text: cef::CefString = text.into();
            host.ime_commit_text(Some(&text), None, 0);
            return Ok(());
        }
        Err(WeldError::PlatformUnsupported("IME requires the cef-runtime feature"))
    }

    fn ime_finish_composing(&mut self, keep_selection: bool) -> Result<(), WeldError> {
        #[cfg(feature = "cef-runtime")]
        if let Some(host) = self.browser.host() {
            host.ime_finish_composing_text(keep_selection as _);
            return Ok(());
        }
        Err(WeldError::PlatformUnsupported("IME requires the cef-runtime feature"))
    }

    fn ime_cancel_composition(&mut self) -> Result<(), WeldError> {
        #[cfg(feature = "cef-runtime")]
        if let Some(host) = self.browser.host() {
            host.ime_cancel_composition();
            return Ok(());
        }
        Err(WeldError::PlatformUnsupported("IME requires the cef-runtime feature"))
    }

    fn set_scale_factor(&mut self, scale: f32) -> Result<(), WeldError> {
        #[cfg(feature = "cef-runtime")]
        {
            self.metrics.lock().unwrap().set_scale(scale);
            // CEF only re-reads GetViewRect / GetScreenInfo when told the view
            // changed, so a scale change has to be announced like a resize or
            // nothing repaints at the new density.
            if let Some(mut host) = self.browser.host() {
                host.notify_screen_info_changed();
                host.was_resized();
                host.invalidate(cef::PaintElementType::default());
            }
            return Ok(());
        }
        #[cfg(not(feature = "cef-runtime"))]
        {
            let _ = scale;
            Err(WeldError::PlatformUnsupported("scale factor requires the cef-runtime feature"))
        }
    }

    fn scale_factor(&self) -> f32 {
        #[cfg(feature = "cef-runtime")]
        {
            return self.metrics.lock().unwrap().scale();
        }
        #[cfg(not(feature = "cef-runtime"))]
        {
            1.0
        }
    }

    fn navigate_to_url(&mut self, url: &str) -> Result<(), WeldError> {
        #[cfg(feature = "cef-runtime")]
        if let Some(mut frame) = self.browser.main_frame() {
            frame.load_url(Some(&url.into()));
            return Ok(());
        }
        Err(pending("cef_frame_t::load_url"))
    }

    fn navigate_to_string(&mut self, content: &str, _mime_type: &str) -> Result<(), WeldError> {
        #[cfg(feature = "cef-runtime")]
        if let Some(mut frame) = self.browser.main_frame() {
            frame.load_url(Some(&content.into()));
            return Ok(());
        }
        Err(pending("cef_frame_t::load_string"))
    }

    fn reload(&mut self) -> Result<(), WeldError> {
        #[cfg(feature = "cef-runtime")]
        { self.browser.reload(); return Ok(()); }
        Err(pending("cef_browser_t::reload"))
    }

    fn stop(&mut self) -> Result<(), WeldError> {
        #[cfg(feature = "cef-runtime")]
        { self.browser.stop_load(); return Ok(()); }
        Err(pending("cef_browser_t::stop_load"))
    }

    fn go_back(&mut self) -> Result<(), WeldError> {
        #[cfg(feature = "cef-runtime")]
        { self.browser.go_back(); return Ok(()); }
        Err(pending("cef_browser_t::go_back"))
    }

    fn go_forward(&mut self) -> Result<(), WeldError> {
        #[cfg(feature = "cef-runtime")]
        { self.browser.go_forward(); return Ok(()); }
        Err(pending("cef_browser_t::go_forward"))
    }

    fn send_mouse_input(&mut self, event: MouseEvent) -> Result<(), WeldError> {
        #[cfg(feature = "cef-runtime")]
        if let Some(host) = self.browser.host() {
            // Hosts speak physical pixels; CEF wants DIP. Skipping this makes
            // every click land at the wrong place on a scaled display.
            let (x, y) = self.metrics.lock().unwrap().point_to_dip(event.x, event.y);
            let event = MouseEvent { x, y, ..event };
            crate::cef_input::send_mouse(&host, &event);
            return Ok(());
        }
        Err(pending("cef_browser_host_t mouse input"))
    }

    fn send_keyboard_input(&mut self, event: KeyEvent) -> Result<(), WeldError> {
        #[cfg(feature = "cef-runtime")]
        if let Some(host) = self.browser.host() {
            crate::cef_input::send_key(&host, &event);
            return Ok(());
        }
        Err(pending("cef_browser_host_t::send_key_event"))
    }

    fn move_focus(&mut self, direction: FocusDirection) -> Result<(), WeldError> {
        #[cfg(feature = "cef-runtime")]
        if let Some(host) = self.browser.host() {
            crate::cef_input::set_focus(&host, direction);
            return Ok(());
        }
        Err(pending("cef_browser_host_t::set_focus"))
    }

    fn post_web_message(&mut self, message: &str) -> Result<(), WeldError> {
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
        if let Some(frame) = self.browser.main_frame() {
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
        self.browser_id
    }

    fn close(&mut self) -> Result<(), WeldError> {
        #[cfg(feature = "cef-runtime")]
        {
            if let Some(mut host) = self.browser.host() {
                host.close_browser(true as _);
            }
            self.browser_id = 0;
            return Ok(());
        }
        self.browser_id = 0;
        Err(pending("cef_browser_host_t::close_browser"))
    }
}

fn pending(op: &'static str) -> WeldError {
    WeldError::BrowserOp(format!("{op}: requires `cef-runtime` feature or pending wiring"))
}

/// Encode `s` as a JSON string literal (double-quoted, backslash-escapes only).
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
