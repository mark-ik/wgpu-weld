// Copyright 2026 Mark Alan Boykin
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0

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
            let mut raw_planes: Vec<(std::os::fd::RawFd, u64, u64, u32)> =
                Vec::with_capacity(plane_count);
            for src in &info.planes[..plane_count] {
                let duped = unsafe { libc::dup(src.fd) };
                if duped < 0 {
                    for (fd, ..) in &raw_planes {
                        unsafe { libc::close(*fd) };
                    }
                    return;
                }
                raw_planes.push((duped, src.offset, src.size, src.stride));
            }

            let width = info.extra.coded_size.width as u32;
            let height = info.extra.coded_size.height as u32;

            let mut slot = if is_popup {
                self.handler.popup_slot.lock().unwrap()
            } else {
                self.handler.frame_slot.lock().unwrap()
            };
            let generation = slot.next_generation();
            let image = unsafe {
                crate::native_frame::DmaBufImage::from_owned_raw_planes(
                    raw_planes,
                    PhysicalSize::new(width, height),
                    format,
                    // drm_format is unused by the Vulkan import path
                    // (which derives vk::Format from wgpu::TextureFormat
                    // directly). Left zeroed for now.
                    0,
                    info.modifier,
                    generation,
                )
            };
            match image {
                Ok(image) => {
                    slot.store(crate::native_frame::NativeFrame::DmaBufImage(image));
                }
                Err(error) => {
                    log::error!("failed to wrap CEF DMABUF image: {error}");
                }
            }
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
                // DmaBufImage's owned buffer table closes duped plane fds.
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

        fn start_dragging(
            &self,
            _browser: Option<&mut cef::Browser>,
            drag_data: Option<&mut cef::DragData>,
            allowed_ops: cef::DragOperationsMask,
            x: ::std::os::raw::c_int,
            y: ::std::os::raw::c_int,
        ) -> ::std::os::raw::c_int {
            let Some(drag_data) = drag_data else { return 0 };
            // CEF gives callback-scoped drag data and DIP coordinates. Copy
            // both into welding-owned values before the host starts its native
            // drag loop, which may outlive this callback by arbitrarily long.
            let scale = self.handler.metrics.lock().unwrap().scale();
            self.handler.events.lock().unwrap().nav.push_back(
                crate::surface::NavigationEvent::DragStarted {
                    payload: crate::drag::payload_from_cef(drag_data),
                    allowed_operations: crate::DragOperations(allowed_ops.as_ref().0 as u32),
                    x: (x as f32 * scale).round() as i32,
                    y: (y as f32 * scale).round() as i32,
                }
            );
            1 // host now owns the system drag loop
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
        download_handler: cef::DownloadHandler,
        permission_handler: cef::PermissionHandler,
        context_menu_handler: cef::ContextMenuHandler,
        find_handler: cef::FindHandler,
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

        fn download_handler(&self) -> Option<cef::DownloadHandler> {
            Some(self.download_handler.clone())
        }

        fn permission_handler(&self) -> Option<cef::PermissionHandler> {
            Some(self.permission_handler.clone())
        }

        fn context_menu_handler(&self) -> Option<cef::ContextMenuHandler> {
            Some(self.context_menu_handler.clone())
        }

        fn find_handler(&self) -> Option<cef::FindHandler> {
            Some(self.find_handler.clone())
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
        download_handler: cef::DownloadHandler,
        permission_handler: cef::PermissionHandler,
        context_menu_handler: cef::ContextMenuHandler,
        find_handler: cef::FindHandler,
        scripts: Arc<crate::app::ScriptResults>,
        events: Arc<Mutex<EventQueues>>,
    ) -> cef::Client {
        Self::new(
            render_handler,
            load_handler,
            display_handler,
            life_span_handler,
            request_handler,
            download_handler,
            permission_handler,
            context_menu_handler,
            find_handler,
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
        auth: Arc<crate::auth::AuthChallenges>,
    }

    impl RequestHandler {
        fn auth_credentials(
            &self,
            _browser: Option<&mut cef::Browser>,
            origin_url: Option<&cef::CefString>,
            is_proxy: ::std::os::raw::c_int,
            host: Option<&cef::CefString>,
            port: ::std::os::raw::c_int,
            realm: Option<&cef::CefString>,
            scheme: Option<&cef::CefString>,
            callback: Option<&mut cef::AuthCallback>,
        ) -> ::std::os::raw::c_int {
            let id = self.auth.next_id();
            let event = crate::surface::NavigationEvent::AuthChallenged {
                id,
                origin_url: origin_url.map(|s| s.to_string()).unwrap_or_default(),
                host: host.map(|s| s.to_string()).unwrap_or_default(),
                port: port.clamp(0, u16::MAX as _) as u16,
                realm: realm.map(|s| s.to_string()).unwrap_or_default(),
                scheme: scheme.map(|s| s.to_string()).unwrap_or_default(),
                is_proxy: is_proxy != 0,
            };
            self.events.lock().unwrap().nav.push_back(event);

            let Some(callback) = callback else { return 0 };
            if self.auth.is_enabled() {
                // Held for the host to answer. CEF's auth callback is
                // reference-counted and may be answered later, unlike the
                // download destination.
                self.auth.hold(id, callback.clone());
            } else {
                // Declined now rather than held: a host that never answers
                // would otherwise keep this request open forever.
                callback.cancel();
            }
            1
        }

        fn on_render_process_terminated(
            &self,
            _browser: Option<&mut cef::Browser>,
            _status: cef::TerminationStatus,
            error_code: ::std::os::raw::c_int,
            error_string: Option<&cef::CefString>,
        ) {
            let status = crate::surface::termination_status(_status);
            let error_string = error_string.map(|s| s.to_string()).unwrap_or_default();
            log::error!(
                "weld: CEF render process terminated ({status:?}, code {error_code}, {error_string})"
            );
            self.events.lock().unwrap().nav.push_back(
                crate::surface::NavigationEvent::ContentProcessTerminated {
                    status,
                    error_code,
                    error_string,
                }
            );
        }
    }
}

impl WeldRequestHandler {
    pub fn build(
        events: Arc<Mutex<EventQueues>>,
        auth: Arc<crate::auth::AuthChallenges>,
    ) -> cef::RequestHandler {
        Self::new(events, auth)
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

// ── Download handler ──────────────────────────────────────────────────────

cef::wrap_download_handler! {
    pub(super) struct WeldDownloadHandler {
        events: Arc<Mutex<EventQueues>>,
        downloads: Arc<crate::downloads::Downloads>,
    }

    impl DownloadHandler {
        fn can_download(
            &self,
            _browser: Option<&mut cef::Browser>,
            _url: Option<&cef::CefString>,
            _request_method: Option<&cef::CefString>,
        ) -> ::std::os::raw::c_int {
            // Refuse before anything is created when there is nowhere to put it,
            // rather than starting a transfer and cancelling it later.
            self.downloads.is_enabled() as _
        }

        fn on_before_download(
            &self,
            _browser: Option<&mut cef::Browser>,
            download_item: Option<&mut cef::DownloadItem>,
            suggested_name: Option<&cef::CefString>,
            callback: Option<&mut cef::BeforeDownloadCallback>,
        ) -> ::std::os::raw::c_int {
            let (Some(item), Some(callback)) = (download_item, callback) else {
                return 0;
            };
            let suggested = suggested_name.map(|s| s.to_string()).unwrap_or_default();
            let Some(destination) = self.downloads.destination_for(&suggested) else {
                // Answering with nothing cancels it, which is what a host
                // without a download directory asked for.
                return 0;
            };
            let id = item.id();
            let url = cef_string(item.url());
            let total = item.total_bytes();
            self.downloads.mark_started(id);
            self.events.lock().unwrap().nav.push_back(
                crate::surface::NavigationEvent::DownloadStarted {
                    id,
                    url,
                    suggested_filename: suggested,
                    destination_path: destination.clone(),
                    total_bytes_expected: (total > 0).then_some(total as u64),
                }
            );
            let path: cef::CefString = destination.to_string_lossy().as_ref().into();
            // false: no system save dialog. The destination is already decided,
            // and a machine nobody is sitting at cannot answer one.
            callback.cont(Some(&path), 0);
            1
        }

        fn on_download_updated(
            &self,
            _browser: Option<&mut cef::Browser>,
            download_item: Option<&mut cef::DownloadItem>,
            callback: Option<&mut cef::DownloadItemCallback>,
        ) {
            let Some(item) = download_item else { return };
            let id = item.id();
            // CEF updates an item before it asks where to put it. Staying quiet
            // until the download has been accepted keeps DownloadStarted first.
            if !self.downloads.has_started(id) {
                return;
            }
            let path = std::path::PathBuf::from(cef_string(item.full_path()));

            // The host's cancel/pause/resume lands here: this callback is the
            // only place CEF offers one, and only for the length of this call.
            if let (Some(callback), Some(op)) = (callback, self.downloads.take_pending(id)) {
                match op {
                    crate::downloads::DownloadOp::Cancel => callback.cancel(),
                    crate::downloads::DownloadOp::Pause => callback.pause(),
                    crate::downloads::DownloadOp::Resume => callback.resume(),
                }
            }

            let total = item.total_bytes();
            let total_bytes_expected = (total > 0).then_some(total as u64);
            let received = item.received_bytes().max(0) as u64;
            let complete = item.is_complete() != 0;
            let canceled = item.is_canceled() != 0;

            let mut events = self.events.lock().unwrap();
            if complete || canceled || self.downloads.due_for_progress(id, std::time::Instant::now()) {
                events.nav.push_back(crate::surface::NavigationEvent::DownloadProgress {
                    id,
                    bytes_received: received,
                    total_bytes_expected,
                });
            }
            if canceled {
                events.nav.push_back(crate::surface::NavigationEvent::DownloadCancelled {
                    id,
                    destination_path: path,
                });
            } else if complete {
                let interrupted = item.is_interrupted() != 0;
                events.nav.push_back(crate::surface::NavigationEvent::DownloadFinished {
                    id,
                    destination_path: path,
                    error: interrupted.then(|| {
                        format!("interrupted: {:?}", *item.interrupt_reason().as_ref() as i32)
                    }),
                });
            }
            drop(events);
            if complete || canceled {
                self.downloads.forget(id);
            }
        }
    }
}

/// CEF hands strings back as `CefStringUserfree`, which is empty when unset.
fn cef_string(s: cef::CefStringUserfree) -> String {
    let raw: Option<&cef::sys::_cef_string_utf16_t> = (&s).into();
    raw.map(|r| cef::CefStringUtf16::from(*r).to_string())
        .unwrap_or_default()
}

impl WeldDownloadHandler {
    pub fn build(
        events: Arc<Mutex<EventQueues>>,
        downloads: Arc<crate::downloads::Downloads>,
    ) -> cef::DownloadHandler {
        Self::new(events, downloads)
    }
}

// ── Permission handler ────────────────────────────────────────────────────

cef::wrap_permission_handler! {
    pub(super) struct WeldPermissionHandler {
        events: Arc<Mutex<EventQueues>>,
        permissions: Arc<crate::permissions::Permissions>,
    }

    impl PermissionHandler {
        fn on_show_permission_prompt(
            &self,
            _browser: Option<&mut cef::Browser>,
            _prompt_id: u64,
            requesting_origin: Option<&cef::CefString>,
            requested_permissions: u32,
            callback: Option<&mut cef::PermissionPromptCallback>,
        ) -> ::std::os::raw::c_int {
            let id = self.permissions.next_id();
            self.events.lock().unwrap().nav.push_back(
                crate::surface::NavigationEvent::PermissionRequested {
                    id,
                    origin: requesting_origin.map(|s| s.to_string()).unwrap_or_default(),
                    permissions: crate::permissions::decode(requested_permissions),
                    raw: requested_permissions,
                }
            );
            let Some(callback) = callback else { return 0 };
            if self.permissions.is_enabled() {
                self.permissions.hold(id, crate::permissions::Pending::Prompt(callback.clone()));
            } else {
                // Denied now rather than held: an unanswered prompt leaves the
                // page waiting forever.
                callback.cont(cef::PermissionRequestResult::DENY);
            }
            1
        }

        fn on_request_media_access_permission(
            &self,
            _browser: Option<&mut cef::Browser>,
            _frame: Option<&mut cef::Frame>,
            requesting_origin: Option<&cef::CefString>,
            requested_permissions: u32,
            callback: Option<&mut cef::MediaAccessCallback>,
        ) -> ::std::os::raw::c_int {
            let id = self.permissions.next_id();
            self.events.lock().unwrap().nav.push_back(
                crate::surface::NavigationEvent::PermissionRequested {
                    id,
                    origin: requesting_origin.map(|s| s.to_string()).unwrap_or_default(),
                    // Media capture arrives on its own callback with its own
                    // bits, so it is decoded from the media set rather than the
                    // prompt set.
                    permissions: crate::permissions::decode_media(requested_permissions),
                    raw: requested_permissions,
                }
            );
            let Some(callback) = callback else { return 0 };
            if self.permissions.is_enabled() {
                self.permissions.hold(
                    id,
                    crate::permissions::Pending::Media(callback.clone(), requested_permissions),
                );
            } else {
                callback.cont(0);
            }
            1
        }
    }
}

impl WeldPermissionHandler {
    pub fn build(
        events: Arc<Mutex<EventQueues>>,
        permissions: Arc<crate::permissions::Permissions>,
    ) -> cef::PermissionHandler {
        Self::new(events, permissions)
    }
}

// ── Context menu handler ──────────────────────────────────────────────────

cef::wrap_context_menu_handler! {
    pub(super) struct WeldContextMenuHandler {
        events: Arc<Mutex<EventQueues>>,
        metrics: Arc<Mutex<crate::view::ViewMetrics>>,
    }

    impl ContextMenuHandler {
        fn on_before_context_menu(
            &self,
            _browser: Option<&mut cef::Browser>,
            _frame: Option<&mut cef::Frame>,
            params: Option<&mut cef::ContextMenuParams>,
            model: Option<&mut cef::MenuModel>,
        ) {
            if let Some(params) = params {
                // CEF reports DIP; every coordinate this API hands out or takes
                // is physical, so convert on the way out.
                let scale = self.metrics.lock().unwrap().scale();
                let flags = params.type_flags().as_ref().0 as u32;
                self.events.lock().unwrap().nav.push_back(
                    crate::surface::NavigationEvent::ContextMenuRequested {
                        x: (params.xcoord() as f32 * scale).round() as i32,
                        y: (params.ycoord() as f32 * scale).round() as i32,
                        targets: crate::surface::context_menu_targets(flags),
                        link_url: cef_string(params.link_url()),
                        source_url: cef_string(params.source_url()),
                        page_url: cef_string(params.page_url()),
                        selection_text: cef_string(params.selection_text()),
                    }
                );
            }
            // Empty the menu. CEF's own has nowhere to draw itself under
            // windowless rendering, so leaving it populated only invites CEF to
            // try; the host draws its own from the event above.
            if let Some(model) = model {
                model.clear();
            }
        }

        fn run_context_menu(
            &self,
            _browser: Option<&mut cef::Browser>,
            _frame: Option<&mut cef::Frame>,
            _params: Option<&mut cef::ContextMenuParams>,
            _model: Option<&mut cef::MenuModel>,
            callback: Option<&mut cef::RunContextMenuCallback>,
        ) -> ::std::os::raw::c_int {
            // Claim it and dismiss immediately: the event has already gone out.
            if let Some(callback) = callback {
                callback.cancel();
            }
            1
        }
    }
}

impl WeldContextMenuHandler {
    pub fn build(
        events: Arc<Mutex<EventQueues>>,
        metrics: Arc<Mutex<crate::view::ViewMetrics>>,
    ) -> cef::ContextMenuHandler {
        Self::new(events, metrics)
    }
}

// ── DevTools protocol observer ────────────────────────────────────────────

cef::wrap_dev_tools_message_observer! {
    pub(super) struct WeldDevToolsObserver {
        channel: Arc<crate::devtools::DevToolsChannel>,
        snapshots: Arc<crate::snapshot::SnapshotChannel>,
    }

    impl DevToolsMessageObserver {
        fn on_dev_tools_message(
            &self,
            _browser: Option<&mut cef::Browser>,
            message: Option<&[u8]>,
        ) -> ::std::os::raw::c_int {
            // The raw wire format, results and events alike. Taking it here
            // rather than from the parsed callbacks keeps what a host sees
            // identical to what the protocol documents.
            if self.channel.is_enabled()
                && let Some(bytes) = message
            {
                self.channel.push(String::from_utf8_lossy(bytes).into_owned());
            }
            // 0: not consumed, so CEF still runs its own parsed callbacks for
            // anything else that wants them.
            0
        }

        fn on_dev_tools_method_result(
            &self,
            _browser: Option<&mut cef::Browser>,
            message_id: ::std::os::raw::c_int,
            success: ::std::os::raw::c_int,
            result: Option<&[u8]>,
        ) {
            self.snapshots.complete(message_id, success != 0, result);
        }
    }
}

impl WeldDevToolsObserver {
    pub fn build(
        channel: Arc<crate::devtools::DevToolsChannel>,
        snapshots: Arc<crate::snapshot::SnapshotChannel>,
    ) -> cef::DevToolsMessageObserver {
        Self::new(channel, snapshots)
    }
}

// ── Find handler ──────────────────────────────────────────────────────────

cef::wrap_find_handler! {
    pub(super) struct WeldFindHandler {
        events: Arc<Mutex<EventQueues>>,
    }

    impl FindHandler {
        fn on_find_result(
            &self,
            _browser: Option<&mut cef::Browser>,
            _identifier: ::std::os::raw::c_int,
            count: ::std::os::raw::c_int,
            _selection_rect: Option<&cef::Rect>,
            active_match_ordinal: ::std::os::raw::c_int,
            final_update: ::std::os::raw::c_int,
        ) {
            self.events.lock().unwrap().nav.push_back(
                crate::surface::NavigationEvent::FindResult {
                    count,
                    active_match: active_match_ordinal,
                    final_update: final_update != 0,
                }
            );
        }
    }
}

impl WeldFindHandler {
    pub fn build(events: Arc<Mutex<EventQueues>>) -> cef::FindHandler {
        Self::new(events)
    }
}

// ── PDF print callback ────────────────────────────────────────────────────

cef::wrap_pdf_print_callback! {
    pub(super) struct WeldPdfCallback {
        events: Arc<Mutex<EventQueues>>,
    }

    impl PdfPrintCallback {
        fn on_pdf_print_finished(
            &self,
            path: Option<&cef::CefString>,
            ok: ::std::os::raw::c_int,
        ) {
            self.events.lock().unwrap().nav.push_back(
                crate::surface::NavigationEvent::PdfPrintFinished {
                    path: path.map(|p| p.to_string()).unwrap_or_default().into(),
                    ok: ok != 0,
                }
            );
        }
    }
}

impl WeldPdfCallback {
    pub fn build(events: Arc<Mutex<EventQueues>>) -> cef::PdfPrintCallback {
        Self::new(events)
    }
}
