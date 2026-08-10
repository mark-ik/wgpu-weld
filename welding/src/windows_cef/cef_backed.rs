//! CEF handler vtables for the Windows producer: render handler (accelerated
//! OSR paint), life-span, client, load, and display handlers.
//!
//! Split out of `windows_cef/mod.rs`; the producer itself stays there.

use super::*;
use cef::*;
use std::ffi::c_void;

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
                rect.x = 0;
                rect.y = 0;
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

        #[cfg(feature = "cef-runtime")]
        fn on_accelerated_paint(
            &self,
            _browser: Option<&mut cef::Browser>,
            type_: cef::PaintElementType,
            _dirty_rects: Option<&[cef::Rect]>,
            info: Option<&cef::AcceleratedPaintInfo>,
        ) {
            // Only handle the VIEW element (not popups).
            if type_ != cef::PaintElementType::default() {
                return;
            }
            let Some(info) = info else { return };
            if info.shared_texture_handle.is_null() {
                return;
            }

            // DuplicateHandle: the original is callback-scoped; we need an
            // owned copy that lives until acquire_frame imports it.
            use windows::Win32::Foundation::{
                DuplicateHandle, DUPLICATE_SAME_ACCESS, HANDLE,
            };
            use windows::Win32::System::Threading::GetCurrentProcess;
            let mut dup = HANDLE::default();
            let ok = unsafe {
                DuplicateHandle(
                    GetCurrentProcess(),
                    HANDLE(info.shared_texture_handle),
                    GetCurrentProcess(),
                    &mut dup,
                    0,
                    false.into(),
                    DUPLICATE_SAME_ACCESS,
                )
                .is_ok()
            };
            if !ok || dup.is_invalid() {
                return;
            }

            let width = info.extra.coded_size.width as u32;
            let height = info.extra.coded_size.height as u32;

            let format = match *info.format.as_ref() {
                cef::sys::cef_color_type_t::CEF_COLOR_TYPE_RGBA_8888 => {
                    wgpu::TextureFormat::Rgba8Unorm
                }
                _ => wgpu::TextureFormat::Bgra8Unorm,
            };

            let generation = self.handler.next_generation.fetch_add(1, Ordering::Relaxed) + 1;
            let frame = Dx12SharedTexture {
                    handle: dup.0 as *mut c_void,
                    size: PhysicalSize::new(width, height),
                    format,
                    generation,
                };
            match WgpuTextureImporter::copy_dx12_callback_frame(
                frame,
                &self.handler.host_ctx,
                &self.handler.callback_copier,
            ) {
                Ok(frame) => {
                    *self.handler.frame_slot.lock().unwrap() = Some(frame);
                }
                Err(err) => {
                    eprintln!("weld: failed to copy CEF accelerated frame: {err}");
                }
            }
        }
    }
}

impl WeldRenderHandler {
    pub fn build(inner: WeldRenderHandlerInner) -> cef::RenderHandler {
        Self::new(inner)
    }
}

cef::wrap_life_span_handler! {
    pub(super) struct WeldLifeSpanHandler {
        state: WeldLifeSpanState,
    }

    impl LifeSpanHandler {
        fn on_after_created(&self, browser: Option<&mut cef::Browser>) {
            let Some(browser) = browser else { return };
            let browser_id = browser.identifier();
            *self.state.browser.lock().unwrap() = Some(browser.clone());
            self.state.browser_id.store(browser_id, Ordering::Release);
            if self.state.close_requested.load(Ordering::Acquire) {
                if let Some(host) = browser.host() {
                    host.close_browser(1);
                }
            }
        }

        fn on_before_close(&self, _browser: Option<&mut cef::Browser>) {
            eprintln!("weld: CEF on_before_close");
            *self.state.browser.lock().unwrap() = None;
            self.state.browser_id.store(0, Ordering::Release);
            self.state.closed.store(true, Ordering::Release);
        }
    }
}

impl WeldLifeSpanHandler {
    pub fn build(state: WeldLifeSpanState) -> cef::LifeSpanHandler {
        Self::new(state)
    }
}

cef::wrap_client! {
    pub(super) struct WeldClient {
        render_handler: cef::RenderHandler,
        life_span_handler: cef::LifeSpanHandler,
        load_handler: cef::LoadHandler,
        display_handler: cef::DisplayHandler,
        events: Arc<Mutex<EventQueues>>,
    }

    impl Client {
        fn render_handler(&self) -> Option<cef::RenderHandler> {
            Some(self.render_handler.clone())
        }

        fn life_span_handler(&self) -> Option<cef::LifeSpanHandler> {
            Some(self.life_span_handler.clone())
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
        life_span_handler: cef::LifeSpanHandler,
        load_handler: cef::LoadHandler,
        display_handler: cef::DisplayHandler,
        events: Arc<Mutex<EventQueues>>,
    ) -> cef::Client {
        Self::new(render_handler, life_span_handler, load_handler, display_handler, events)
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
