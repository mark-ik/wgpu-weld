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
use cef::{
    ImplAuthCallback, ImplBrowser, ImplBrowserHost, ImplFrame, ImplListValue,
    ImplMediaAccessCallback, ImplPermissionPromptCallback, ImplProcessMessage,
};

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
    #[cfg(feature = "cef-runtime")]
    cookies: Arc<crate::cookies::CookieJar>,
    downloads: Arc<crate::downloads::Downloads>,
    auth: Arc<crate::auth::AuthChallenges>,
    permissions: Arc<crate::permissions::Permissions>,
    devtools: Arc<crate::devtools::DevToolsChannel>,
    /// Keeps the CDP subscription alive: dropping the registration
    /// unsubscribes, so the observer must outlive the producer's interest.
    #[cfg(feature = "cef-runtime")]
    _devtools_registration: Option<cef::Registration>,
    #[cfg(feature = "cef-runtime")]
    scripts: Arc<crate::app::ScriptResults>,
    #[cfg(feature = "cef-runtime")]
    next_script_id: u32,
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
            let cookies = Arc::new(crate::cookies::CookieJar::default());
            let downloads = Arc::new(crate::downloads::Downloads::default());
            let auth = Arc::new(crate::auth::AuthChallenges::default());
            auth.set_enabled(config.surface.handle_auth_challenges);
            let scripts = Arc::new(crate::app::ScriptResults::default());

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
            let request_handler = cef_backed::WeldRequestHandler::build(events.clone(), auth.clone());
            downloads.set_dir(config.surface.download_dir.clone());
            let download_handler =
                cef_backed::WeldDownloadHandler::build(events.clone(), downloads.clone());
            let devtools = Arc::new(crate::devtools::DevToolsChannel::default());
            devtools.set_enabled(config.surface.devtools_protocol);
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
                cookies,
                downloads,
                auth,
                permissions,
                devtools,
                _devtools_registration: None,
                scripts,
                next_script_id: 0,
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
    fn ensure_devtools(&mut self) -> Result<(), WeldError> {
        if !self.devtools.is_enabled() {
            return Err(WeldError::PlatformUnsupported(
                "set CefSurfaceConfig::devtools_protocol to use the DevTools protocol",
            ));
        }
        if self._devtools_registration.is_some() {
            return Ok(());
        }
        let Some(host) = self.browser.host() else {
            return Err(WeldError::PlatformUnsupported(
                "the browser is not ready yet",
            ));
        };
        let mut observer = cef_backed::WeldDevToolsObserver::build(self.devtools.clone());
        self._devtools_registration = host.add_dev_tools_message_observer(Some(&mut observer));
        Ok(())
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

    fn request_script_result(&mut self, script: &str) -> Result<u32, WeldError> {
        #[cfg(feature = "cef-runtime")]
        {
            self.next_script_id = self.next_script_id.wrapping_add(1);
            let id = self.next_script_id;
            let Some(frame) = self.browser.main_frame() else {
                return Err(WeldError::BrowserOp("browser has no main frame yet".into()));
            };
            let name: cef::CefString = crate::app::EVAL_REQUEST.into();
            let mut message = cef::process_message_create(Some(&name))
                .ok_or_else(|| WeldError::BrowserOp("could not create a process message".into()))?;
            if let Some(args) = message.argument_list() {
                args.set_int(0, id as i32);
                let script: cef::CefString = script.into();
                args.set_string(1, Some(&script));
            }
            frame.send_process_message(cef::ProcessId::RENDERER, Some(&mut message));
            return Ok(id);
        }
        #[cfg(not(feature = "cef-runtime"))]
        {
            let _ = script;
            Err(WeldError::PlatformUnsupported("script results require the cef-runtime feature"))
        }
    }

    fn poll_script_result(&mut self) -> Option<crate::app::ScriptResult> {
        #[cfg(feature = "cef-runtime")]
        {
            return self.scripts.take_one();
        }
        #[cfg(not(feature = "cef-runtime"))]
        {
            None
        }
    }

    fn set_cookie(&mut self, url: &str, cookie: &crate::surface::Cookie) -> Result<(), WeldError> {
        #[cfg(feature = "cef-runtime")]
        {
            return crate::cookies::set(url, cookie);
        }
        #[cfg(not(feature = "cef-runtime"))]
        {
            let _ = (url, cookie);
            Err(WeldError::PlatformUnsupported("cookies require the cef-runtime feature"))
        }
    }

    fn request_cookies(&mut self, url: Option<&str>) -> Result<(), WeldError> {
        #[cfg(feature = "cef-runtime")]
        {
            return crate::cookies::request(&self.cookies, url);
        }
        #[cfg(not(feature = "cef-runtime"))]
        {
            let _ = url;
            Err(WeldError::PlatformUnsupported("cookies require the cef-runtime feature"))
        }
    }

    fn poll_cookies(&mut self) -> Option<Vec<crate::surface::Cookie>> {
        #[cfg(feature = "cef-runtime")]
        {
            return self.cookies.take();
        }
        #[cfg(not(feature = "cef-runtime"))]
        {
            None
        }
    }

    fn delete_cookies(&mut self, url: Option<&str>, name: Option<&str>) -> Result<(), WeldError> {
        #[cfg(feature = "cef-runtime")]
        {
            return crate::cookies::delete(url, name);
        }
        #[cfg(not(feature = "cef-runtime"))]
        {
            let _ = (url, name);
            Err(WeldError::PlatformUnsupported("cookies require the cef-runtime feature"))
        }
    }

    fn set_visible(&mut self, visible: bool) -> Result<(), WeldError> {
        #[cfg(feature = "cef-runtime")]
        if let Some(host) = self.browser.host() {
            host.was_hidden(if visible { 0 } else { 1 });
            return Ok(());
        }
        Err(WeldError::PlatformUnsupported("visibility requires the cef-runtime feature"))
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
            // `replacement_range` must be a real pointer, never None. CEF's own
            // C++ wrapper takes it by reference and so always passes one, which
            // makes non-null the C API's contract; libcef's generated entry
            // point verifies it and returns early on NULL. That return is
            // silent in a release build, so passing None here did not fail --
            // it dropped every composition before CEF saw it. UINT32_MAX twice
            // is the invalid range that means "replace nothing", which is what
            // cefclient passes.
            let no_replacement = cef::Range { from: u32::MAX, to: u32::MAX };
            // No underlines: CEF renders the composition inside the page, and
            // the default styling is what a page author expects.
            host.ime_set_composition(
                Some(&text),
                None,
                Some(&no_replacement),
                Some(&selection),
            );
            return Ok(());
        }
        Err(WeldError::PlatformUnsupported("IME requires the cef-runtime feature"))
    }

    fn ime_commit_text(&mut self, text: &str) -> Result<(), WeldError> {
        #[cfg(feature = "cef-runtime")]
        if let Some(host) = self.browser.host() {
            let text: cef::CefString = text.into();
            // Same contract as ime_set_composition: a NULL replacement_range is
            // dropped silently by libcef's entry point.
            let no_replacement = cef::Range { from: u32::MAX, to: u32::MAX };
            host.ime_commit_text(Some(&text), Some(&no_replacement), 0);
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

    fn request_repaint(&mut self) -> Result<(), WeldError> {
        #[cfg(feature = "cef-runtime")]
        {
            if let Some(mut host) = self.browser.host() {
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
                return Err(WeldError::PlatformUnsupported("the browser is not ready yet"));
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
        { self.browser.reload(); return Ok(()); }
        Err(pending("cef_browser_t::reload"))
    }

    fn stop(&mut self) -> Result<(), WeldError> {
        #[cfg(feature = "cef-runtime")]
        { self.browser.stop_load(); return Ok(()); }
        Err(pending("cef_browser_t::stop_load"))
    }

    fn can_go_back(&self) -> bool {
        #[cfg(feature = "cef-runtime")]
        {
            return Some(&self.browser).map(|b| b.can_go_back() != 0).unwrap_or(false);
        }
        #[cfg(not(feature = "cef-runtime"))]
        false
    }

    fn can_go_forward(&self) -> bool {
        #[cfg(feature = "cef-runtime")]
        {
            return Some(&self.browser).map(|b| b.can_go_forward() != 0).unwrap_or(false);
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

    fn find(&mut self, text: &str, forward: bool, match_case: bool, find_next: bool)
        -> Result<(), WeldError>
    {
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
            {
                let m = self.metrics.lock().unwrap();
                log::debug!(
                    "input {:?}: physical {},{} -> dip {},{} (scale {}, view {:?} dip)",
                    event.action, event.x, event.y, x, y, m.scale(), m.logical()
                );
            }
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
        #[cfg(feature = "cef-runtime")]
        {
            if let Some(host) = self.browser.host() {
                // A real WindowInfo, not None. CEF dereferences it on macOS:
                // passing None crashed the host inside the framework with
                // EXC_BAD_ACCESS at null+0x150, while Windows tolerated it and
                // opened the window -- exactly the kind of difference that
                // ships if only one platform is tried. Windowless is left off
                // so CEF opens DevTools in its own native window; a windowless
                // DevTools would need a producer of its own.
                let window_info = cef::WindowInfo {
                    bounds: cef::Rect { x: 0, y: 0, width: 1024, height: 768 },
                    ..Default::default()
                };
                let settings = cef::BrowserSettings::default();
                // `inspect_element_at` must be a real pointer too. CEF's C++ API
                // takes it as `const CefPoint&` and its generated entry point
                // lists it "unverified", meaning libcef dereferences it without
                // a null check -- which is how a None here became a crash inside
                // the framework on macOS rather than an error. (0,0) is the
                // "inspect nothing in particular" value.
                let inspect_at = cef::Point { x: 0, y: 0 };
                host.show_dev_tools(
                    Some(&window_info),
                    None,
                    Some(&settings),
                    Some(&inspect_at),
                );
                return Ok(());
            }
        }
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
