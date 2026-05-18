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
    events: Arc<Mutex<EventQueues>>,
    size: Arc<Mutex<PhysicalSize<u32>>>,
}

// ── cef-runtime: render handler + client ─────────────────────────────────────

#[cfg(feature = "cef-runtime")]
mod cef_backed {
    use super::*;
    use cef::*;

    cef::wrap_render_handler! {
        pub(super) struct WeldRenderHandler {
            handler: WeldRenderHandlerInner,
        }

        impl RenderHandler {
            fn view_rect(
                &self,
                _browser: Option<&mut cef::Browser>,
                rect: Option<&mut cef::Rect>,
            ) {
                if let Some(rect) = rect {
                    let size = self.handler.size.lock().unwrap();
                    rect.width = size.width as _;
                    rect.height = size.height as _;
                }
            }

            fn screen_info(
                &self,
                _browser: Option<&mut cef::Browser>,
                screen_info: Option<&mut cef::ScreenInfo>,
            ) -> ::std::os::raw::c_int {
                if let Some(info) = screen_info {
                    info.device_scale_factor = 1.0;
                    return 1;
                }
                0
            }

            fn screen_point(
                &self,
                _browser: Option<&mut cef::Browser>,
                _view_x: ::std::os::raw::c_int,
                _view_y: ::std::os::raw::c_int,
                _screen_x: Option<&mut ::std::os::raw::c_int>,
                _screen_y: Option<&mut ::std::os::raw::c_int>,
            ) -> ::std::os::raw::c_int {
                0
            }

            fn on_accelerated_paint(
                &self,
                _browser: Option<&mut cef::Browser>,
                type_: cef::PaintElementType,
                _dirty_rects: Option<&[cef::Rect]>,
                info: Option<&cef::AcceleratedPaintInfo>,
            ) {
                // VIEW element only; skip popup paints.
                if type_ != cef::PaintElementType::default() {
                    return;
                }
                let Some(info) = info else { return };

                let plane_count = info.plane_count as usize;
                log::debug!(
                    "on_accelerated_paint: planes={}, format={:?}, modifier=0x{:x}, coded_size={}x{}",
                    plane_count,
                    info.format,
                    info.modifier,
                    info.extra.coded_size.width,
                    info.extra.coded_size.height,
                );
                if plane_count == 0 || plane_count > info.planes.len() {
                    return;
                }

                // Map CEF color type → wgpu sRGB texture format. CEF emits
                // sRGB pixel data; importing as UNORM would double-apply
                // gamma at sample time. See cef#3687 thread.
                let format = match *info.format.as_ref() {
                    cef::sys::cef_color_type_t::CEF_COLOR_TYPE_RGBA_8888 => {
                        wgpu::TextureFormat::Rgba8UnormSrgb
                    }
                    _ => wgpu::TextureFormat::Bgra8UnormSrgb,
                };

                // dup(2) every plane fd. CEF closes the originals after the
                // callback returns; Vulkan will close ours on successful
                // import. On dup failure we close any fds we already duped.
                let mut planes: Vec<crate::native_frame::DmaBufPlane> =
                    Vec::with_capacity(plane_count);
                for src in &info.planes[..plane_count] {
                    let duped = unsafe { libc::dup(src.fd) };
                    if duped < 0 {
                        for p in &planes {
                            unsafe { libc::close(p.fd) };
                        }
                        return;
                    }
                    planes.push(crate::native_frame::DmaBufPlane {
                        fd: duped,
                        offset: src.offset,
                        size: src.size,
                        stride: src.stride,
                    });
                }

                let width = info.extra.coded_size.width as u32;
                let height = info.extra.coded_size.height as u32;

                let mut slot = self.handler.frame_slot.lock().unwrap();
                let generation = slot.next_generation();
                slot.store(crate::native_frame::NativeFrame::DmaBufImage(
                    crate::native_frame::DmaBufImage {
                        planes,
                        size: PhysicalSize::new(width, height),
                        format,
                        // drm_format is unused by the Vulkan import path
                        // (which derives vk::Format from wgpu::TextureFormat
                        // directly). Left zeroed for now.
                        drm_format: 0,
                        modifier: info.modifier,
                        generation,
                    },
                ));
            }
        }
    }

    impl WeldRenderHandler {
        pub fn build(inner: WeldRenderHandlerInner) -> cef::RenderHandler {
            Self::new(inner)
        }
    }

    cef::wrap_client! {
        pub(super) struct WeldClient {
            render_handler: cef::RenderHandler,
            load_handler: cef::LoadHandler,
            display_handler: cef::DisplayHandler,
            events: Arc<Mutex<EventQueues>>,
        }

        impl Client {
            fn render_handler(&self) -> Option<cef::RenderHandler> {
                Some(self.render_handler.clone())
            }

            fn load_handler(&self) -> Option<cef::LoadHandler> {
                Some(self.load_handler.clone())
            }

            fn display_handler(&self) -> Option<cef::DisplayHandler> {
                Some(self.display_handler.clone())
            }

            fn on_process_message_received(
                &self,
                _browser: Option<&mut cef::Browser>,
                _frame: Option<&mut cef::Frame>,
                source_process: cef::ProcessId,
                message: Option<&mut cef::ProcessMessage>,
            ) -> ::std::os::raw::c_int {
                if source_process != cef::ProcessId::RENDERER { return 0; }
                let Some(msg) = message else { return 0 };
                if cef::CefString::from(&msg.name()).to_string() != "weld.message" { return 0; }
                if let Some(args) = msg.argument_list() {
                    let text = cef::CefString::from(&args.string(0)).to_string();
                    self.events.lock().unwrap().web_messages.push_back(text);
                }
                1
            }
        }
    }

    impl WeldClient {
        pub fn build(
            render_handler: cef::RenderHandler,
            load_handler: cef::LoadHandler,
            display_handler: cef::DisplayHandler,
            events: Arc<Mutex<EventQueues>>,
        ) -> cef::Client {
            Self::new(render_handler, load_handler, display_handler, events)
        }
    }

    // ── Load handler ─────────────────────────────────────────────────────────

    cef::wrap_load_handler! {
        pub(super) struct WeldLoadHandler {
            inner: WeldRenderHandlerInner,
        }

        impl LoadHandler {
            fn on_load_start(
                &self,
                _browser: Option<&mut cef::Browser>,
                frame: Option<&mut cef::Frame>,
                _transition_type: cef::TransitionType,
            ) {
                let is_main = frame.as_ref().map(|f| f.is_main() != 0).unwrap_or(false);
                if !is_main { return; }
                let url = frame.map(|f| cef::CefString::from(&f.url()).to_string()).unwrap_or_default();
                self.inner.events.lock().unwrap().nav.push_back(
                    crate::surface::NavigationEvent::LoadStart { url }
                );
            }

            fn on_load_end(
                &self,
                _browser: Option<&mut cef::Browser>,
                frame: Option<&mut cef::Frame>,
                http_status_code: ::std::os::raw::c_int,
            ) {
                let is_main = frame.as_ref().map(|f| f.is_main() != 0).unwrap_or(false);
                if !is_main { return; }
                let url = frame.map(|f| cef::CefString::from(&f.url()).to_string()).unwrap_or_default();
                self.inner.events.lock().unwrap().nav.push_back(
                    crate::surface::NavigationEvent::LoadEnd {
                        url,
                        http_status: http_status_code,
                    }
                );
            }

            fn on_load_error(
                &self,
                _browser: Option<&mut cef::Browser>,
                frame: Option<&mut cef::Frame>,
                _error_code: cef::Errorcode,
                error_text: Option<&cef::CefString>,
                failed_url: Option<&cef::CefString>,
            ) {
                let is_main = frame.as_ref().map(|f| f.is_main() != 0).unwrap_or(false);
                if !is_main { return; }
                let url = failed_url.map(|u| u.to_string()).unwrap_or_default();
                let text = error_text.map(|t| t.to_string()).unwrap_or_default();
                // Safety: Errorcode wraps cef_errorcode_t which wraps c_int;
                // both are #[repr(transparent)] around a 4-byte integer.
                let code: i32 = unsafe { std::mem::transmute(_error_code) };
                self.inner.events.lock().unwrap().nav.push_back(
                    crate::surface::NavigationEvent::LoadError {
                        url,
                        error_code: code,
                        error_text: text,
                    }
                );
            }
        }
    }

    impl WeldLoadHandler {
        pub fn build(inner: WeldRenderHandlerInner) -> cef::LoadHandler {
            Self::new(inner)
        }
    }

    // ── Display handler ───────────────────────────────────────────────────────

    cef::wrap_display_handler! {
        pub(super) struct WeldDisplayHandler {
            inner: WeldRenderHandlerInner,
        }

        impl DisplayHandler {
            fn on_address_change(
                &self,
                _browser: Option<&mut cef::Browser>,
                frame: Option<&mut cef::Frame>,
                url: Option<&cef::CefString>,
            ) {
                let is_main = frame.as_ref().map(|f| f.is_main() != 0).unwrap_or(false);
                if !is_main { return; }
                let url = url.map(|u| u.to_string()).unwrap_or_default();
                self.inner.events.lock().unwrap().nav.push_back(
                    crate::surface::NavigationEvent::AddressChanged { url }
                );
            }

            fn on_title_change(
                &self,
                _browser: Option<&mut cef::Browser>,
                title: Option<&cef::CefString>,
            ) {
                let title = title.map(|t| t.to_string()).unwrap_or_default();
                self.inner.events.lock().unwrap().nav.push_back(
                    crate::surface::NavigationEvent::TitleChanged { title }
                );
            }
        }
    }

    impl WeldDisplayHandler {
        pub fn build(inner: WeldRenderHandlerInner) -> cef::DisplayHandler {
            Self::new(inner)
        }
    }
}

// ── Producer struct ───────────────────────────────────────────────────────────

pub struct LinuxCefProducer {
    browser_id: i32,
    #[cfg(feature = "cef-runtime")]
    browser: cef::Browser,
    #[cfg(feature = "cef-runtime")]
    cef_size: Arc<Mutex<PhysicalSize<u32>>>,
    frame_slot: Arc<Mutex<PendingFrameSlot>>,
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
            let cef_size = Arc::new(Mutex::new(initial_size));

            let inner = WeldRenderHandlerInner {
                frame_slot: frame_slot.clone(),
                events: events.clone(),
                size: cef_size.clone(),
            };
            let render_handler = cef_backed::WeldRenderHandler::build(inner.clone());
            let load_handler = cef_backed::WeldLoadHandler::build(inner.clone());
            let display_handler = cef_backed::WeldDisplayHandler::build(inner);
            let mut client = cef_backed::WeldClient::build(
                render_handler,
                load_handler,
                display_handler,
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
                cef_size,
                frame_slot,
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

    fn resize(&mut self, size: PhysicalSize<u32>) -> Result<(), WeldError> {
        self.size = size;
        #[cfg(feature = "cef-runtime")]
        {
            *self.cef_size.lock().unwrap() = size;
            if let Some(mut host) = self.browser.host() {
                host.was_resized();
            }
            return Ok(());
        }
        Err(pending("cef_browser_host_t::was_resized"))
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
