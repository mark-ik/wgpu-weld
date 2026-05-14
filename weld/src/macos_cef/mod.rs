/// macOS CEF producer: accelerated OSR via `OnAcceleratedPaint` / `IOSurfaceRef`.
///
/// # IOSurface retain / release contract
///
/// `CefAcceleratedPaintInfo::shared_texture_handle` on macOS is an
/// `IOSurfaceRef` (`*mut c_void`). Under `cef-runtime` the `on_accelerated_paint`
/// callback retains the surface via `IOSurfaceIncrementUseCount` before the
/// callback returns, and the Metal importer releases it after wrapping it in a
/// `MTLTexture`.
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

#[cfg(not(feature = "cef-runtime"))]
use crate::cef_ffi::CefFunctions;

// ── Public config ─────────────────────────────────────────────────────────────

pub struct MacosCefConfig {
    pub surface: CefSurfaceConfig,
}

impl Default for MacosCefConfig {
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
    use std::ffi::c_void;

    cef::wrap_render_handler! {
        pub(super) struct WeldRenderHandler {
            handler: WeldRenderHandlerInner,
        }

        impl cef::RenderHandler {
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

            #[cfg(feature = "cef-runtime")]
            fn on_accelerated_paint(
                &self,
                _browser: Option<&mut cef::Browser>,
                type_: cef::PaintElementType,
                _dirty_rects: Option<&[cef::Rect]>,
                info: Option<&cef::AcceleratedPaintInfo>,
            ) {
                if type_ != cef::PaintElementType::default() {
                    return;
                }
                let Some(info) = info else { return };
                let io_surface = info.shared_texture_handle;
                if io_surface.is_null() {
                    return;
                }

                // Retain the IOSurface so it outlives the callback.
                // IOSurfaceRef is a CFTypeRef; use CFRetain.
                extern "C" {
                    fn CFRetain(cf: *const c_void) -> *const c_void;
                }
                unsafe { CFRetain(io_surface) };

                let width = info.extra.coded_size.width as u32;
                let height = info.extra.coded_size.height as u32;
                let format = match *info.format.as_ref() {
                    cef::sys::cef_color_type_t::CEF_COLOR_TYPE_RGBA_8888 => {
                        wgpu::TextureFormat::Rgba8Unorm
                    }
                    _ => wgpu::TextureFormat::Bgra8Unorm,
                };

                let mut slot = self.handler.frame_slot.lock().unwrap();
                let generation = slot.next_generation();
                slot.store(crate::native_frame::NativeFrame::MetalTextureRef(
                    crate::native_frame::MetalTextureRef {
                        io_surface: io_surface as *mut c_void,
                        size: PhysicalSize::new(width, height),
                        format,
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
        }

        impl cef::Client {
            fn render_handler(&self) -> Option<cef::RenderHandler> {
                Some(self.render_handler.clone())
            }
        }
    }

    impl WeldClient {
        pub fn build(render_handler: cef::RenderHandler) -> cef::Client {
            Self::new(render_handler)
        }
    }
}

// ── Producer struct ───────────────────────────────────────────────────────────

pub struct MacosCefProducer {
    #[cfg(not(feature = "cef-runtime"))]
    _fns: Arc<CefFunctions>,
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
unsafe impl Send for MacosCefProducer {}

impl MacosCefProducer {
    pub fn new(runtime: &CefRuntime, config: MacosCefConfig) -> Result<Self, WeldError> {
        #[cfg(feature = "cef-runtime")]
        {
            let initial_size = config.surface.initial_size;
            let frame_slot = Arc::new(Mutex::new(PendingFrameSlot::default()));
            let events = Arc::new(Mutex::new(EventQueues {
                nav: VecDeque::new(),
                web_messages: VecDeque::new(),
            }));
            let cef_size = Arc::new(Mutex::new(initial_size));

            let inner = cef_backed::WeldRenderHandlerInner {
                frame_slot: frame_slot.clone(),
                events: events.clone(),
                size: cef_size.clone(),
            };
            let render_handler = cef_backed::WeldRenderHandler::build(inner);
            let mut client = cef_backed::WeldClient::build(render_handler);

            let window_info = cef::WindowInfo {
                windowless_rendering_enabled: 1,
                shared_texture_enabled: 1,
                external_begin_frame_enabled: 1,
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
            return Ok(MacosCefProducer {
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
            let _ = (runtime, config);
            Err(WeldError::SurfaceCreation(
                "macOS CEF vtable wiring requires the `cef-runtime` feature".into(),
            ))
        }
    }
}

// ── CefSurfaceProducer impl ───────────────────────────────────────────────────

impl CefSurfaceProducer for MacosCefProducer {
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

    fn send_mouse_input(&mut self, _event: MouseEvent) -> Result<(), WeldError> {
        Err(pending("cef_browser_host_t mouse input"))
    }

    fn send_keyboard_input(&mut self, _event: KeyEvent) -> Result<(), WeldError> {
        Err(pending("cef_browser_host_t::send_key_event"))
    }

    fn move_focus(&mut self, _direction: FocusDirection) -> Result<(), WeldError> {
        Err(pending("cef_browser_host_t::set_focus/move_focus"))
    }

    fn post_web_message(&mut self, _message: &str) -> Result<(), WeldError> {
        Err(pending("cef_frame_t::send_process_message"))
    }

    fn poll_web_message(&mut self) -> Option<String> {
        self.events.lock().unwrap().web_messages.pop_front()
    }

    fn poll_navigation_event(&mut self) -> Option<NavigationEvent> {
        self.events.lock().unwrap().nav.pop_front()
    }

    fn execute_script(&mut self, _script: &str, _source_url: &str) -> Result<(), WeldError> {
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
