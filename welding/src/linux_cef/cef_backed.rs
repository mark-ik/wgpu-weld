//! CEF handler vtables for the Linux producer: render handler (accelerated
//! OSR paint), life-span, client, load, and display handlers.
//!
//! Split out of `linux_cef/mod.rs`; the producer itself stays there.

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
                // GetViewRect is answered in DIP, not physical pixels.
                let (w, h) = self.handler.metrics.lock().unwrap().logical();
                log::trace!("view_rect -> {w}x{h} dip");
                rect.width = w;
                rect.height = h;
            }
        }

        fn screen_info(
            &self,
            _browser: Option<&mut cef::Browser>,
            screen_info: Option<&mut cef::ScreenInfo>,
        ) -> ::std::os::raw::c_int {
            if let Some(info) = screen_info {
                info.device_scale_factor = self.handler.metrics.lock().unwrap().scale();
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
            // CEF paints the popup widget (select dropdowns, autocomplete) as
            // its own element rather than compositing it into the view, so it
            // lands here with PET_POPUP and goes to a separate slot.
            let is_popup = type_ == cef::PaintElementType::POPUP;
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

            let mut slot = if is_popup {
                self.handler.popup_slot.lock().unwrap()
            } else {
                self.handler.frame_slot.lock().unwrap()
            };
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

        fn on_ime_composition_range_changed(
            &self,
            _browser: Option<&mut cef::Browser>,
            selected_range: Option<&cef::Range>,
            character_bounds: Option<&[cef::Rect]>,
        ) {
            let rects: Vec<crate::surface::PopupRect> = character_bounds
                .unwrap_or(&[])
                .iter()
                .map(|r| crate::surface::PopupRect {
                    x: r.x,
                    y: r.y,
                    width: r.width.max(0) as u32,
                    height: r.height.max(0) as u32,
                })
                .collect();
            let Some(bounds) = crate::ime::bounds_union(&rects) else { return };
            // CEF reports these in DIP, like popup geometry.
            let bounds = self.handler.metrics.lock().unwrap().rect_to_physical(bounds);
            let (start, end) = selected_range
                .map(|r| (r.from, r.to))
                .unwrap_or((0, 0));
            self.handler.ime.set(crate::surface::ImeComposition {
                bounds,
                selection_start: start,
                selection_end: end,
            });
        }

        fn on_popup_show(&self, _browser: Option<&mut cef::Browser>, show: ::std::os::raw::c_int) {
            let showing = show != 0;
            log::debug!("on_popup_show({showing})");
            self.handler.popup.set_visible(showing);
            if !showing {
                // A hidden popup never paints again; drop the stale surface so
                // acquire_popup cannot hand back a dropdown that is gone.
                // DmaBufImage::Drop closes the duped plane fds.
                let _ = self.handler.popup_slot.lock().unwrap().take();
            }
        }

        fn on_popup_size(&self, _browser: Option<&mut cef::Browser>, rect: Option<&cef::Rect>) {
            let Some(rect) = rect else { return };
            log::debug!(
                "on_popup_size({}x{} at {},{})",
                rect.width, rect.height, rect.x, rect.y
            );
            self.handler.popup.set_rect(crate::surface::PopupRect {
                x: rect.x,
                y: rect.y,
                width: rect.width.max(0) as u32,
                height: rect.height.max(0) as u32,
            });
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
        life_span_handler: cef::LifeSpanHandler,
        request_handler: cef::RequestHandler,
        scripts: Arc<crate::app::ScriptResults>,
        events: Arc<Mutex<EventQueues>>,
    }

    impl Client {
        fn render_handler(&self) -> Option<cef::RenderHandler> {
            Some(self.render_handler.clone())
        }

        fn load_handler(&self) -> Option<cef::LoadHandler> {
            Some(self.load_handler.clone())
        }

        fn life_span_handler(&self) -> Option<cef::LifeSpanHandler> {
            Some(self.life_span_handler.clone())
        }

        fn request_handler(&self) -> Option<cef::RequestHandler> {
            Some(self.request_handler.clone())
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
            let name = cef::CefString::from(&msg.name()).to_string();
            if name == crate::app::EVAL_RESULT {
                // The renderer answering a request_script_result.
                if let Some(args) = msg.argument_list() {
                    let id = args.int(0) as u32;
                    let ok = args.int(1) != 0;
                    let payload = cef::CefString::from(&args.string(2)).to_string();
                    self.scripts.push(crate::app::ScriptResult {
                        id,
                        value: if ok { Ok(payload) } else { Err(payload) },
                    });
                }
                return 1;
            }
            if name != "weld.message" { return 0; }
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
        life_span_handler: cef::LifeSpanHandler,
        request_handler: cef::RequestHandler,
        scripts: Arc<crate::app::ScriptResults>,
        events: Arc<Mutex<EventQueues>>,
    ) -> cef::Client {
        Self::new(
            render_handler,
            load_handler,
            display_handler,
            life_span_handler,
            request_handler,
            scripts,
            events,
        )
    }
}

// ── Life-span handler: popup policy ───────────────────────────────────────

cef::wrap_life_span_handler! {
    pub(super) struct WeldLifeSpanHandler {
        events: Arc<Mutex<EventQueues>>,
    }

    impl LifeSpanHandler {
        // Popup browsers are denied and reported. welding renders one surface
        // per producer, so a second browser here would be invisible to the
        // host. The host decides what to do with the URL.
        #[allow(clippy::too_many_arguments)]
        fn on_before_popup(
            &self,
            _browser: Option<&mut cef::Browser>,
            _frame: Option<&mut cef::Frame>,
            _popup_id: ::std::os::raw::c_int,
            target_url: Option<&cef::CefString>,
            _target_frame_name: Option<&cef::CefString>,
            _target_disposition: cef::WindowOpenDisposition,
            user_gesture: ::std::os::raw::c_int,
            _popup_features: Option<&cef::PopupFeatures>,
            _window_info: Option<&mut cef::WindowInfo>,
            _client: Option<&mut Option<cef::Client>>,
            _settings: Option<&mut cef::BrowserSettings>,
            _extra_info: Option<&mut Option<cef::DictionaryValue>>,
            _no_javascript_access: Option<&mut ::std::os::raw::c_int>,
        ) -> ::std::os::raw::c_int {
            let url = target_url.map(|u| u.to_string()).unwrap_or_default();
            self.events.lock().unwrap().nav.push_back(
                crate::surface::NavigationEvent::NewWindowRequested {
                    url,
                    user_gesture: user_gesture != 0,
                }
            );
            1 // cancel popup creation
        }
    }
}

impl WeldLifeSpanHandler {
    pub fn build(events: Arc<Mutex<EventQueues>>) -> cef::LifeSpanHandler {
        Self::new(events)
    }
}

// ── Request handler ───────────────────────────────────────────────────────

cef::wrap_request_handler! {
    pub(super) struct WeldRequestHandler {
        events: Arc<Mutex<EventQueues>>,
    }

    impl RequestHandler {
        fn on_render_process_terminated(
            &self,
            _browser: Option<&mut cef::Browser>,
            _status: cef::TerminationStatus,
            error_code: ::std::os::raw::c_int,
            error_string: Option<&cef::CefString>,
        ) {
            log::error!(
                "weld: CEF render process terminated (code {error_code}, {})",
                error_string.map(|s| s.to_string()).unwrap_or_default()
            );
            self.events.lock().unwrap().nav.push_back(
                crate::surface::NavigationEvent::ContentProcessTerminated
            );
        }
    }
}

impl WeldRequestHandler {
    pub fn build(events: Arc<Mutex<EventQueues>>) -> cef::RequestHandler {
        Self::new(events)
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

        // CEF puts OnCursorChange on the display handler, not the render
        // handler, and types the handle differently per platform (::std::os::raw::c_ulong here).
        fn on_cursor_change(
            &self,
            _browser: Option<&mut cef::Browser>,
            _cursor: ::std::os::raw::c_ulong,
            type_: cef::CursorType,
            _custom_cursor_info: Option<&cef::CursorInfo>,
        ) -> ::std::os::raw::c_int {
            self.inner.cursor.set(crate::cursor::from_cef(type_));
            1 // handled: under OSR the host owns the pointer
        }

        fn on_console_message(
            &self,
            _browser: Option<&mut cef::Browser>,
            level: cef::LogSeverity,
            message: Option<&cef::CefString>,
            source: Option<&cef::CefString>,
            line: ::std::os::raw::c_int,
        ) -> ::std::os::raw::c_int {
            self.inner.events.lock().unwrap().nav.push_back(
                crate::surface::NavigationEvent::ConsoleMessage {
                    // cef_log_severity_t is repr(u32) here and repr(i32) on
                    // Windows, so go through the reference rather than From.
                    level: *level.as_ref() as i32,
                    message: message.map(|m| m.to_string()).unwrap_or_default(),
                    source: source.map(|s| s.to_string()).unwrap_or_default(),
                    line,
                }
            );
            0 // let CEF log it as well
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
