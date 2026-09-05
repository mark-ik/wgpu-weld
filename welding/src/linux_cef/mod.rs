// Copyright 2026 Mark Alan Boykin
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0

//! Linux CEF producer: accelerated OSR via native pixmap / DMABUF planes
//! and Vulkan external memory.
//!
//! # DMABUF fd lifetime
//!
//! Each plane fd in `AcceleratedPaintInfo` is callback-scoped. The
//! `on_accelerated_paint` callback calls `dup(2)` on every fd before storing
//! the planes in [`DmaBufImage`](crate::native_frame::DmaBufImage). The Vulkan
//! importer takes ownership of the duped fds on success; otherwise the image's
//! owned descriptor buffer table closes them.

use std::sync::{Arc, Mutex};

use dpi::PhysicalSize;

use crate::{
    error::WeldError,
    native_frame::{
        HostWgpuContext, ImportedTexture, NativeFrame, PendingFrameSlot, WgpuTextureImporter,
    },
    runtime::CefRuntime,
    surface::{
        CefSurfaceConfig, CefSurfaceEvent, CefSurfaceMode, CefSurfaceProducer, FocusDirection,
        KeyEvent, MouseEvent, WebEventQueue, WebRequestId,
    },
};

#[cfg(feature = "cef-runtime")]
use cef::{
    ImplAuthCallback, ImplBrowser, ImplBrowserHost, ImplFrame, ImplListValue,
    ImplMediaAccessCallback, ImplPermissionPromptCallback, ImplProcessMessage,
};

// ── Public config ─────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct LinuxCefConfig {
    pub surface: CefSurfaceConfig,
}

// ── Shared callback state ─────────────────────────────────────────────────────

#[cfg(feature = "cef-runtime")]
#[derive(Clone)]
struct WeldRenderHandlerInner {
    frame_slot: Arc<Mutex<PendingFrameSlot>>,
    popup_slot: Arc<Mutex<PendingFrameSlot>>,
    popup: Arc<crate::popup::PopupState>,
    cursor: Arc<crate::cursor::LatestCursor>,
    ime: Arc<crate::ime::LatestComposition>,
    events: Arc<WebEventQueue>,
    metrics: Arc<Mutex<crate::view::ViewMetrics>>,
}

// ── cef-runtime: render handler + client ─────────────────────────────────────

// too_many_arguments: CEF vtable glue takes one argument per handler; the
// signatures are dictated by CEF, and a params struct would carry the same names.
#[allow(clippy::too_many_arguments)]
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
    #[cfg(feature = "cef-runtime")]
    cookies: Arc<crate::cookies::CookieJar>,
    #[cfg(feature = "cef-runtime")]
    downloads: Arc<crate::downloads::Downloads>,
    #[cfg(feature = "cef-runtime")]
    auth: Arc<crate::auth::AuthChallenges>,
    #[cfg(feature = "cef-runtime")]
    permissions: Arc<crate::permissions::Permissions>,
    #[cfg(feature = "cef-runtime")]
    devtools: Arc<crate::devtools::DevToolsChannel>,
    #[cfg(feature = "cef-runtime")]
    snapshots: Arc<crate::snapshot::SnapshotChannel>,
    /// Keeps the CDP subscription alive: dropping the registration
    /// unsubscribes, so the observer must outlive the producer's interest.
    #[cfg(feature = "cef-runtime")]
    _devtools_registration: Option<cef::Registration>,
    #[cfg(feature = "cef-runtime")]
    scripts: Arc<crate::app::PendingScripts>,
    #[cfg(feature = "cef-runtime")]
    next_snapshot_id: i32,
    events: Arc<WebEventQueue>,
    size: PhysicalSize<u32>,
}

// needless_return: cfg-dispatch bodies end in `return X;` because a cfg-gated
// scaffold block follows; when the runtime arm is the only one compiled the
// return looks needless to clippy but the idiom requires it.
#[allow(clippy::needless_return)]
impl LinuxCefProducer {
    pub fn new(_runtime: &CefRuntime, config: LinuxCefConfig) -> Result<Self, WeldError> {
        #[cfg(feature = "cef-runtime")]
        {
            let initial_size = config.surface.initial_size;
            let frame_slot = Arc::new(Mutex::new(PendingFrameSlot::default()));
            let events = Arc::new(WebEventQueue::default());
            let metrics = Arc::new(Mutex::new(crate::view::ViewMetrics::new(
                initial_size,
                config.surface.scale_factor,
            )));

            let popup_slot = Arc::new(Mutex::new(PendingFrameSlot::default()));
            let popup = Arc::new(crate::popup::PopupState::default());
            let cursor = Arc::new(crate::cursor::LatestCursor::default());
            let ime = Arc::new(crate::ime::LatestComposition::default());
            let cookies = Arc::new(crate::cookies::CookieJar::new(events.clone()));
            let downloads = Arc::new(crate::downloads::Downloads::default());
            let auth = Arc::new(crate::auth::AuthChallenges::default());
            auth.set_enabled(config.surface.handle_auth_challenges);
            let scripts = Arc::new(crate::app::PendingScripts::new(events.clone()));

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
            let life_span_handler = cef_backed::WeldLifeSpanHandler::build(
                events.clone(),
                scripts.clone(),
                cookies.clone(),
            );
            let request_handler = cef_backed::WeldRequestHandler::build(
                events.clone(),
                auth.clone(),
                scripts.clone(),
            );
            downloads.set_dir(config.surface.download_dir.clone());
            let download_handler =
                cef_backed::WeldDownloadHandler::build(events.clone(), downloads.clone());
            let devtools = Arc::new(crate::devtools::DevToolsChannel::default());
            devtools.set_enabled(config.surface.devtools_protocol);
            let snapshots = Arc::new(crate::snapshot::SnapshotChannel::default());
            let permissions = Arc::new(crate::permissions::Permissions::default());
            permissions.set_enabled(config.surface.handle_permission_requests);
            let permission_handler =
                cef_backed::WeldPermissionHandler::build(events.clone(), permissions.clone());
            let context_menu_handler =
                cef_backed::WeldContextMenuHandler::build(events.clone(), metrics.clone());
            let find_handler = cef_backed::WeldFindHandler::build(events.clone());
            let mut client = cef_backed::WeldClient::build(
                render_handler,
                load_handler,
                display_handler,
                life_span_handler,
                request_handler,
                download_handler,
                permission_handler,
                context_menu_handler,
                find_handler,
                scripts.clone(),
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
                background_color: config.surface.cef_background_color(),
                ..Default::default()
            };
            let url: cef::CefString = config.surface.initial_url.as_str().into();
            // Passing no request context selects CEF's process-global profile.
            // Build one even for the in-memory case so every producer owns its
            // cookies, storage, and permission decisions.
            let mut request_context = crate::profile::create(_runtime, &config.surface)?;
            // A disk-backed child context completes its initialization through
            // the host loop before it can create a browser.
            _runtime.do_message_loop_work();

            let browser = cef::browser_host_create_browser_sync(
                Some(&window_info),
                Some(&mut client),
                Some(&url),
                Some(&browser_settings),
                None,
                Some(&mut request_context),
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
                cookies,
                downloads,
                auth,
                permissions,
                devtools,
                snapshots,
                _devtools_registration: None,
                scripts,
                next_snapshot_id: 1,
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

// unreachable_code: cfg-gated fallback Errs are unreachable on the cef-runtime path.
// unused_variables/mut: parameters used only in cfg(cef-runtime) branches appear unused on scaffold path.
// needless_return: cfg-dispatch bodies end in `return X;` so the scaffold block can follow.
#[allow(
    unreachable_code,
    unused_mut,
    unused_variables,
    clippy::needless_return
)]
impl LinuxCefProducer {
    /// Answer a held permission request. Media hands back the subset being
    /// granted, everything else an accept/deny result.
    fn answer_permission(
        &mut self,
        id: crate::PermissionId,
        granted: bool,
    ) -> Result<(), WeldError> {
        #[cfg(feature = "cef-runtime")]
        {
            let Some(pending) = self.permissions.take(id) else {
                return Err(WeldError::PlatformUnsupported(
                    "no permission request is waiting on that id",
                ));
            };
            match pending {
                crate::permissions::Pending::Prompt(callback) => {
                    callback.cont(if granted {
                        cef::PermissionRequestResult::ACCEPT
                    } else {
                        cef::PermissionRequestResult::DENY
                    });
                }
                crate::permissions::Pending::Media(callback, requested) => {
                    callback.cont(if granted { requested } else { 0 });
                }
            }
            return Ok(());
        }
        #[cfg(not(feature = "cef-runtime"))]
        {
            let _ = (id, granted);
            Err(pending("cef_permission_prompt_callback_t"))
        }
    }

    /// Subscribe to CDP if asked and not already subscribed.
    ///
    /// Lazy on purpose: the browser is created asynchronously, so there is no
    /// host to register against until it exists. Dropping the registration
    /// unsubscribes, so it is kept on the producer.
    #[cfg(feature = "cef-runtime")]
    fn ensure_devtools_observer(&mut self) -> Result<(), WeldError> {
        if self._devtools_registration.is_some() {
            return Ok(());
        }
        let Some(host) = self.browser.host() else {
            return Err(WeldError::PlatformUnsupported(
                "the browser is not ready yet",
            ));
        };
        let mut observer =
            cef_backed::WeldDevToolsObserver::build(self.devtools.clone(), self.snapshots.clone());
        self._devtools_registration = host.add_dev_tools_message_observer(Some(&mut observer));
        Ok(())
    }

    #[cfg(feature = "cef-runtime")]
    fn ensure_devtools(&mut self) -> Result<(), WeldError> {
        if !self.devtools.is_enabled() {
            return Err(WeldError::PlatformUnsupported(
                "set CefSurfaceConfig::devtools_protocol to use the DevTools protocol",
            ));
        }
        self.ensure_devtools_observer()
    }

    /// Record a download request. It is applied on that download's next
    /// update, because CEF's download callback exists only inside one.
    fn request_download(
        &mut self,
        id: crate::DownloadId,
        op: crate::downloads::DownloadOp,
    ) -> Result<(), WeldError> {
        #[cfg(feature = "cef-runtime")]
        {
            if !self.downloads.is_enabled() {
                return Err(WeldError::PlatformUnsupported(
                    "no download_dir is configured, so there are no downloads to steer",
                ));
            }
            self.downloads.request(id, op);
            return Ok(());
        }
        #[cfg(not(feature = "cef-runtime"))]
        {
            let _ = (id, op);
            Err(pending("cef_download_item_callback_t"))
        }
    }
}

// unreachable_code: shared fallback tails are unreachable on the cef-runtime path.
// unused_variables: parameters used only in cfg(cef-runtime) branches appear unused on scaffold path.
// needless_return: cfg-dispatch bodies end in `return X;` so the scaffold block can follow.
#[allow(unreachable_code, unused_variables, clippy::needless_return)]
impl CefSurfaceProducer for LinuxCefProducer {
    fn surface_mode(&self) -> CefSurfaceMode {
        CefSurfaceMode::AcceleratedPaint
    }

    fn acquire_native_frame(&mut self) -> Option<NativeFrame> {
        self.frame_slot.lock().unwrap().take()
    }

    fn acquire_frame(
        &mut self,
        ctx: &HostWgpuContext,
    ) -> Result<Option<ImportedTexture>, WeldError> {
        match self.acquire_native_frame() {
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
            if let Some(host) = self.browser.host() {
                host.was_resized();
            }
            return Ok(());
        }
        Err(pending("cef_browser_host_t::was_resized"))
    }

    fn request_script_result(&mut self, id: WebRequestId, script: &str) -> Result<(), WeldError> {
        #[cfg(feature = "cef-runtime")]
        {
            let Some(frame) = self.browser.main_frame() else {
                return Err(WeldError::BrowserOp("browser has no main frame yet".into()));
            };
            let name: cef::CefString = crate::app::EVAL_REQUEST.into();
            let Some(mut message) = cef::process_message_create(Some(&name)) else {
                return Err(WeldError::BrowserOp(
                    "could not create a process message".into(),
                ));
            };
            let Some(args) = message.argument_list() else {
                return Err(WeldError::BrowserOp(
                    "process message has no argument list".into(),
                ));
            };
            let id_text = id.to_string();
            let id_text: cef::CefString = id_text.as_str().into();
            args.set_string(0, Some(&id_text));
            let script: cef::CefString = script.into();
            args.set_string(1, Some(&script));
            self.scripts.begin(id)?;
            if frame.send_process_message(cef::ProcessId::RENDERER, Some(&mut message)) == 0 {
                self.scripts.abort(id);
                return Err(WeldError::BrowserOp(
                    "CEF rejected the script process message".into(),
                ));
            }
            return Ok(());
        }
        #[cfg(not(feature = "cef-runtime"))]
        {
            let _ = (id, script);
            Err(WeldError::PlatformUnsupported(
                "script results require the cef-runtime feature",
            ))
        }
    }

    fn set_cookie(&mut self, url: &str, cookie: &crate::surface::Cookie) -> Result<(), WeldError> {
        #[cfg(feature = "cef-runtime")]
        {
            return crate::cookies::set(&self.browser, url, cookie);
        }
        #[cfg(not(feature = "cef-runtime"))]
        {
            let _ = (url, cookie);
            Err(WeldError::PlatformUnsupported(
                "cookies require the cef-runtime feature",
            ))
        }
    }

    fn request_cookies(&mut self, id: WebRequestId, url: Option<&str>) -> Result<(), WeldError> {
        #[cfg(feature = "cef-runtime")]
        {
            return crate::cookies::request(&self.browser, &self.cookies, id, url);
        }
        #[cfg(not(feature = "cef-runtime"))]
        {
            let _ = (id, url);
            Err(WeldError::PlatformUnsupported(
                "cookies require the cef-runtime feature",
            ))
        }
    }

    fn delete_cookies(&mut self, url: Option<&str>, name: Option<&str>) -> Result<(), WeldError> {
        #[cfg(feature = "cef-runtime")]
        {
            return crate::cookies::delete(&self.browser, url, name);
        }
        #[cfg(not(feature = "cef-runtime"))]
        {
            let _ = (url, name);
            Err(WeldError::PlatformUnsupported(
                "cookies require the cef-runtime feature",
            ))
        }
    }

    fn set_visible(&mut self, visible: bool) -> Result<(), WeldError> {
        #[cfg(feature = "cef-runtime")]
        if let Some(host) = self.browser.host() {
            host.was_hidden(if visible { 0 } else { 1 });
            return Ok(());
        }
        Err(WeldError::PlatformUnsupported(
            "visibility requires the cef-runtime feature",
        ))
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
            let selection = cef::Range {
                from: selection.0,
                to: selection.1,
            };
            // `replacement_range` must be a real pointer, never None. CEF's own
            // C++ wrapper takes it by reference and so always passes one, which
            // makes non-null the C API's contract; libcef's generated entry
            // point verifies it and returns early on NULL. That return is
            // silent in a release build, so passing None here did not fail --
            // it dropped every composition before CEF saw it. UINT32_MAX twice
            // is the invalid range that means "replace nothing", which is what
            // cefclient passes.
            let no_replacement = cef::Range {
                from: u32::MAX,
                to: u32::MAX,
            };
            // No underlines: CEF renders the composition inside the page, and
            // the default styling is what a page author expects.
            host.ime_set_composition(Some(&text), None, Some(&no_replacement), Some(&selection));
            return Ok(());
        }
        Err(WeldError::PlatformUnsupported(
            "IME requires the cef-runtime feature",
        ))
    }

    fn ime_commit_text(&mut self, text: &str) -> Result<(), WeldError> {
        #[cfg(feature = "cef-runtime")]
        if let Some(host) = self.browser.host() {
            let text: cef::CefString = text.into();
            // Same contract as ime_set_composition: a NULL replacement_range is
            // dropped silently by libcef's entry point.
            let no_replacement = cef::Range {
                from: u32::MAX,
                to: u32::MAX,
            };
            host.ime_commit_text(Some(&text), Some(&no_replacement), 0);
            return Ok(());
        }
        Err(WeldError::PlatformUnsupported(
            "IME requires the cef-runtime feature",
        ))
    }

    fn ime_finish_composing(&mut self, keep_selection: bool) -> Result<(), WeldError> {
        #[cfg(feature = "cef-runtime")]
        if let Some(host) = self.browser.host() {
            host.ime_finish_composing_text(keep_selection as _);
            return Ok(());
        }
        Err(WeldError::PlatformUnsupported(
            "IME requires the cef-runtime feature",
        ))
    }

    fn ime_cancel_composition(&mut self) -> Result<(), WeldError> {
        #[cfg(feature = "cef-runtime")]
        if let Some(host) = self.browser.host() {
            host.ime_cancel_composition();
            return Ok(());
        }
        Err(WeldError::PlatformUnsupported(
            "IME requires the cef-runtime feature",
        ))
    }

    fn set_scale_factor(&mut self, scale: f32) -> Result<(), WeldError> {
        #[cfg(feature = "cef-runtime")]
        {
            self.metrics.lock().unwrap().set_scale(scale);
            // CEF only re-reads GetViewRect / GetScreenInfo when told the view
            // changed, so a scale change has to be announced like a resize or
            // nothing repaints at the new density.
            if let Some(host) = self.browser.host() {
                host.notify_screen_info_changed();
                host.was_resized();
                host.invalidate(cef::PaintElementType::default());
            }
            return Ok(());
        }
        #[cfg(not(feature = "cef-runtime"))]
        {
            let _ = scale;
            Err(WeldError::PlatformUnsupported(
                "scale factor requires the cef-runtime feature",
            ))
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
        if let Some(frame) = self.browser.main_frame() {
            frame.load_url(Some(&url.into()));
            return Ok(());
        }
        Err(pending("cef_frame_t::load_url"))
    }

    fn navigate_to_string(&mut self, content: &str, _mime_type: &str) -> Result<(), WeldError> {
        #[cfg(feature = "cef-runtime")]
        if let Some(frame) = self.browser.main_frame() {
            frame.load_url(Some(&content.into()));
            return Ok(());
        }
        Err(pending("cef_frame_t::load_string"))
    }

    fn request_repaint(&mut self) -> Result<(), WeldError> {
        #[cfg(feature = "cef-runtime")]
        {
            if let Some(host) = self.browser.host() {
                // The same pair `resize` uses: was_resized makes CEF re-query
                // the view rect, invalidate asks for the paint itself.
                host.was_resized();
                host.invalidate(cef::PaintElementType::default());
                return Ok(());
            }
        }
        Err(pending("cef_browser_host_t::invalidate"))
    }

    fn send_devtools_message(&mut self, json: &str) -> Result<(), WeldError> {
        #[cfg(feature = "cef-runtime")]
        {
            self.ensure_devtools()?;
            let Some(host) = self.browser.host() else {
                return Err(WeldError::PlatformUnsupported(
                    "the browser is not ready yet",
                ));
            };
            // The wire format goes straight through, unparsed.
            host.send_dev_tools_message(Some(json.as_bytes()));
            return Ok(());
        }
        #[cfg(not(feature = "cef-runtime"))]
        {
            let _ = json;
            Err(pending("cef_browser_host_t::send_dev_tools_message"))
        }
    }

    fn poll_devtools_message(&mut self) -> Option<String> {
        #[cfg(feature = "cef-runtime")]
        {
            // Subscribing here too, so a host that only listens still receives.
            let _ = self.ensure_devtools();
            return self.devtools.pop();
        }
        #[cfg(not(feature = "cef-runtime"))]
        None
    }

    fn devtools_dropped(&self) -> u64 {
        #[cfg(feature = "cef-runtime")]
        {
            return self.devtools.dropped();
        }
        #[cfg(not(feature = "cef-runtime"))]
        0
    }

    fn grant_permission(&mut self, id: crate::PermissionId) -> Result<(), WeldError> {
        self.answer_permission(id, true)
    }

    fn deny_permission(&mut self, id: crate::PermissionId) -> Result<(), WeldError> {
        self.answer_permission(id, false)
    }

    fn answer_auth(
        &mut self,
        id: crate::AuthId,
        username: &str,
        password: &str,
    ) -> Result<(), WeldError> {
        #[cfg(feature = "cef-runtime")]
        {
            // Deliberately not logged, here or anywhere.
            let Some(callback) = self.auth.take(id) else {
                return Err(WeldError::PlatformUnsupported(
                    "no auth challenge is waiting on that id",
                ));
            };
            let user: cef::CefString = username.into();
            let pass: cef::CefString = password.into();
            callback.cont(Some(&user), Some(&pass));
            return Ok(());
        }
        #[cfg(not(feature = "cef-runtime"))]
        {
            let _ = (id, username, password);
            Err(pending("cef_auth_callback_t::cont"))
        }
    }

    fn cancel_auth(&mut self, id: crate::AuthId) -> Result<(), WeldError> {
        #[cfg(feature = "cef-runtime")]
        {
            let Some(callback) = self.auth.take(id) else {
                return Err(WeldError::PlatformUnsupported(
                    "no auth challenge is waiting on that id",
                ));
            };
            callback.cancel();
            return Ok(());
        }
        #[cfg(not(feature = "cef-runtime"))]
        {
            let _ = id;
            Err(pending("cef_auth_callback_t::cancel"))
        }
    }

    fn cancel_download(&mut self, id: crate::DownloadId) -> Result<(), WeldError> {
        self.request_download(id, crate::downloads::DownloadOp::Cancel)
    }

    fn pause_download(&mut self, id: crate::DownloadId) -> Result<(), WeldError> {
        self.request_download(id, crate::downloads::DownloadOp::Pause)
    }

    fn resume_download(&mut self, id: crate::DownloadId) -> Result<(), WeldError> {
        self.request_download(id, crate::downloads::DownloadOp::Resume)
    }

    fn reload(&mut self) -> Result<(), WeldError> {
        #[cfg(feature = "cef-runtime")]
        {
            self.browser.reload();
            return Ok(());
        }
        Err(pending("cef_browser_t::reload"))
    }

    fn stop(&mut self) -> Result<(), WeldError> {
        #[cfg(feature = "cef-runtime")]
        {
            self.browser.stop_load();
            return Ok(());
        }
        Err(pending("cef_browser_t::stop_load"))
    }

    fn can_go_back(&self) -> bool {
        #[cfg(feature = "cef-runtime")]
        {
            return Some(&self.browser)
                .map(|b| b.can_go_back() != 0)
                .unwrap_or(false);
        }
        #[cfg(not(feature = "cef-runtime"))]
        false
    }

    fn can_go_forward(&self) -> bool {
        #[cfg(feature = "cef-runtime")]
        {
            return Some(&self.browser)
                .map(|b| b.can_go_forward() != 0)
                .unwrap_or(false);
        }
        #[cfg(not(feature = "cef-runtime"))]
        false
    }

    fn zoom(&mut self, command: crate::ZoomCommand) -> Result<(), WeldError> {
        #[cfg(feature = "cef-runtime")]
        if let Some(host) = self.browser.host() {
            host.zoom(match command {
                crate::ZoomCommand::In => cef::ZoomCommand::IN,
                crate::ZoomCommand::Out => cef::ZoomCommand::OUT,
                crate::ZoomCommand::Reset => cef::ZoomCommand::RESET,
            });
            return Ok(());
        }
        #[cfg(not(feature = "cef-runtime"))]
        let _ = command;
        Err(pending("cef_browser_host_t::zoom"))
    }

    fn set_zoom_level(&mut self, level: f64) -> Result<(), WeldError> {
        #[cfg(feature = "cef-runtime")]
        if let Some(host) = self.browser.host() {
            host.set_zoom_level(level);
            return Ok(());
        }
        #[cfg(not(feature = "cef-runtime"))]
        let _ = level;
        Err(pending("cef_browser_host_t::set_zoom_level"))
    }

    fn zoom_level(&self) -> f64 {
        #[cfg(feature = "cef-runtime")]
        {
            return self.browser.host().map(|h| h.zoom_level()).unwrap_or(0.0);
        }
        #[cfg(not(feature = "cef-runtime"))]
        0.0
    }

    fn print_to_pdf(&mut self, path: &std::path::Path) -> Result<(), WeldError> {
        #[cfg(feature = "cef-runtime")]
        if let Some(host) = self.browser.host() {
            let path_str: cef::CefString = path.to_string_lossy().as_ref().into();
            let mut callback = cef_backed::WeldPdfCallback::build(self.events.clone());
            // Default settings: Chromium's own page size and margins, which is
            // what a host that did not ask for anything else expects.
            let settings = cef::PdfPrintSettings::default();
            host.print_to_pdf(Some(&path_str), Some(&settings), Some(&mut callback));
            return Ok(());
        }
        #[cfg(not(feature = "cef-runtime"))]
        let _ = path;
        Err(pending("cef_browser_host_t::print_to_pdf"))
    }

    fn print(&mut self) -> Result<(), WeldError> {
        // Unlike Windows and macOS, CEF on Linux has no built-in print UI.
        // Calling BrowserHost::Print without a CefPrintHandler simply claims a
        // capability it cannot carry through to an actual system printer.
        Err(WeldError::PlatformUnsupported(
            "Linux CEF requires an embedder-owned print handler and printer UI",
        ))
    }

    fn request_snapshot_png(&mut self) -> Result<crate::SnapshotRequestId, WeldError> {
        #[cfg(feature = "cef-runtime")]
        {
            self.ensure_devtools_observer()?;
            let Some(host) = self.browser.host() else {
                return Err(WeldError::PlatformUnsupported(
                    "the browser is not ready yet",
                ));
            };
            let id = crate::SnapshotRequestId::from_cef_message_id(self.next_snapshot_id);
            self.next_snapshot_id = self.next_snapshot_id.checked_add(1).unwrap_or(1);
            self.snapshots.begin(id)?;
            let method: cef::CefString = "Page.captureScreenshot".into();
            // `None` is meaningful: Page.captureScreenshot defaults to PNG.
            // Ignore the immediate return: CEF documents it as meaningful only
            // on the UI thread, and Windows intentionally runs that thread
            // separately. The observer callback is the actual receipt.
            let _ = host.execute_dev_tools_method(id.cef_message_id(), Some(&method), None);
            return Ok(id);
        }
        Err(pending("cef_browser_host_t::execute_dev_tools_method"))
    }

    fn poll_snapshot_png(&mut self) -> Option<crate::SnapshotPngCompletion> {
        #[cfg(feature = "cef-runtime")]
        {
            return self.snapshots.take();
        }
        #[cfg(not(feature = "cef-runtime"))]
        None
    }

    fn find(
        &mut self,
        text: &str,
        forward: bool,
        match_case: bool,
        find_next: bool,
    ) -> Result<(), WeldError> {
        #[cfg(feature = "cef-runtime")]
        if let Some(host) = self.browser.host() {
            let text: cef::CefString = text.into();
            host.find(Some(&text), forward as _, match_case as _, find_next as _);
            return Ok(());
        }
        #[cfg(not(feature = "cef-runtime"))]
        let _ = (text, forward, match_case, find_next);
        Err(pending("cef_browser_host_t::find"))
    }

    fn stop_finding(&mut self, clear_selection: bool) -> Result<(), WeldError> {
        #[cfg(feature = "cef-runtime")]
        if let Some(host) = self.browser.host() {
            host.stop_finding(clear_selection as _);
            return Ok(());
        }
        #[cfg(not(feature = "cef-runtime"))]
        let _ = clear_selection;
        Err(pending("cef_browser_host_t::stop_finding"))
    }

    fn go_back(&mut self) -> Result<(), WeldError> {
        #[cfg(feature = "cef-runtime")]
        {
            self.browser.go_back();
            return Ok(());
        }
        Err(pending("cef_browser_t::go_back"))
    }

    fn go_forward(&mut self) -> Result<(), WeldError> {
        #[cfg(feature = "cef-runtime")]
        {
            self.browser.go_forward();
            return Ok(());
        }
        Err(pending("cef_browser_t::go_forward"))
    }

    fn send_mouse_input(&mut self, event: MouseEvent) -> Result<(), WeldError> {
        #[cfg(feature = "cef-runtime")]
        if let Some(host) = self.browser.host() {
            // Hosts speak physical pixels; CEF wants DIP. Skipping this makes
            // every click land at the wrong place on a scaled display.
            let (x, y) = self.metrics.lock().unwrap().point_to_dip(event.x, event.y);
            {
                let m = self.metrics.lock().unwrap();
                log::debug!(
                    "input {:?}: physical {},{} -> dip {},{} (scale {}, view {:?} dip)",
                    event.action,
                    event.x,
                    event.y,
                    x,
                    y,
                    m.scale(),
                    m.logical()
                );
            }
            let event = MouseEvent { x, y, ..event };
            crate::cef_input::send_mouse(&host, &event);
            return Ok(());
        }
        Err(pending("cef_browser_host_t mouse input"))
    }

    fn send_touch_input(&mut self, event: crate::TouchInput) -> Result<(), WeldError> {
        #[cfg(feature = "cef-runtime")]
        if let Some(host) = self.browser.host() {
            let scale = self.metrics.lock().unwrap().scale();
            crate::drag::send_touch(&host, event, scale);
            return Ok(());
        }
        Err(pending("cef_browser_host_t::send_touch_event"))
    }

    fn send_drag_input(&mut self, event: crate::DragInput) -> Result<(), WeldError> {
        #[cfg(feature = "cef-runtime")]
        if let Some(host) = self.browser.host() {
            let scale = self.metrics.lock().unwrap().scale();
            return crate::drag::send_drag(&host, event, scale);
        }
        Err(pending("cef_browser_host_t drag target input"))
    }

    fn finish_drag_source(
        &mut self,
        x: i32,
        y: i32,
        operation: crate::DragOperations,
    ) -> Result<(), WeldError> {
        #[cfg(feature = "cef-runtime")]
        if let Some(host) = self.browser.host() {
            let scale = self.metrics.lock().unwrap().scale();
            crate::drag::finish_drag_source(&host, x, y, operation, scale);
            return Ok(());
        }
        Err(pending("cef_browser_host_t drag source completion"))
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

    fn poll_web_event(&mut self) -> Option<CefSurfaceEvent> {
        self.events.poll()
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
        Err(WeldError::PlatformUnsupported(
            crate::surface::CEF_OSR_DEVTOOLS_REASON,
        ))
    }

    fn browser_id(&self) -> i32 {
        self.browser_id
    }

    fn close(&mut self) -> Result<(), WeldError> {
        #[cfg(feature = "cef-runtime")]
        {
            self.scripts.fail_all("browser closed");
            self.cookies.fail_active("browser closed");
            if let Some(host) = self.browser.host() {
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
    WeldError::BrowserOp(format!(
        "{op}: requires `cef-runtime` feature or pending wiring"
    ))
}

/// Encode `s` as a JSON string literal (double-quoted, backslash-escapes only).
#[rustfmt::skip]
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
