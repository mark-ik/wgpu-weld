/// macOS CEF producer: accelerated OSR via `OnAcceleratedPaint` / IOSurface.
///
/// # IOSurface retain / release contract
///
/// `CefAcceleratedPaintInfo::shared_texture_handle` on macOS is an
/// `IOSurfaceRef` (`CFTypeRef` / `*mut c_void`). CEF hands it to the callback
/// already retained; **`MacosCefProducer` must retain it again** (via
/// `IOSurfaceIncrementUseCount` / `CFRetain`) before the callback returns, and
/// release it after `acquire_frame` imports it into a Metal-backed wgpu texture.
///
/// Unlike the Windows path there is no `DuplicateHandle` — the IOSurface is
/// ref-counted and safe to hold across the callback boundary as long as the
/// retain is taken before return.
///
/// # CEF vtable wiring (not yet implemented)
///
/// Same pattern as `windows_cef`: a `cef_client_t` + `cef_render_handler_t`
/// vtable pair whose `OnAcceleratedPaint` callback retains the `IOSurfaceRef`
/// and stores it in an `Arc<FrameSlot>`.
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use dpi::PhysicalSize;

use crate::{
    cef_ffi::CefFunctions,
    error::WeldError,
    native_frame::{HostWgpuContext, ImportedTexture, MetalTextureRef, NativeFrame, WgpuTextureImporter},
    runtime::CefRuntime,
    surface::{
        CefSurfaceConfig, CefSurfaceMode, CefSurfaceProducer,
        FocusDirection, KeyEvent, MouseEvent, NavigationEvent,
    },
};

// ── Public config ─────────────────────────────────────────────────────────────

pub struct MacosCefConfig {
    pub surface: CefSurfaceConfig,
}

impl Default for MacosCefConfig {
    fn default() -> Self {
        MacosCefConfig { surface: CefSurfaceConfig::default() }
    }
}

// ── Shared callback state ─────────────────────────────────────────────────────

/// Written by the `OnAcceleratedPaint` callback; read by `acquire_frame`.
/// The `io_surface` pointer is retained before it is stored here and
/// released after import in `WgpuTextureImporter::import_metal`.
struct FrameSlot {
    frame: Option<NativeFrame>,
    width: u32,
    height: u32,
}

struct EventQueues {
    nav: VecDeque<NavigationEvent>,
    web_messages: VecDeque<String>,
}

// ── Producer ──────────────────────────────────────────────────────────────────

pub struct MacosCefProducer {
    fns: Arc<CefFunctions>,
    browser_id: i32,
    frame_slot: Arc<Mutex<FrameSlot>>,
    events: Arc<Mutex<EventQueues>>,
    size: PhysicalSize<u32>,
}

impl MacosCefProducer {
    /// Create a CEF browser in OSR (windowless + shared-texture) mode on macOS.
    ///
    /// # IOSurface retain in `OnAcceleratedPaint`
    ///
    /// The vtable wiring must do the following inside the callback:
    ///
    /// ```text
    /// // Retain the IOSurface so it outlives the callback.
    /// // IOSurfaceRef is a CFTypeRef; use CFRetain.
    /// CFRetain(info.shared_texture_handle);
    /// *frame_slot.lock() = Some(NativeFrame::MetalTextureRef(
    ///     MetalTextureRef {
    ///         io_surface: info.shared_texture_handle,  // now retained
    ///         width,
    ///         height,
    ///     }
    /// ));
    /// ```
    ///
    /// `WgpuTextureImporter::import_metal` calls `CFRelease` after the Metal
    /// texture is created from the IOSurface.
    pub fn new(runtime: &CefRuntime, config: MacosCefConfig) -> Result<Self, WeldError> {
        let frame_slot = Arc::new(Mutex::new(FrameSlot {
            frame: None,
            width: config.surface.initial_size.width,
            height: config.surface.initial_size.height,
        }));
        let events = Arc::new(Mutex::new(EventQueues {
            nav: VecDeque::new(),
            web_messages: VecDeque::new(),
        }));
        let _fns = runtime.fns();
        todo!("wire cef_client_t + cef_render_handler_t vtables; create browser")
    }
}

impl CefSurfaceProducer for MacosCefProducer {
    fn surface_mode(&self) -> CefSurfaceMode {
        CefSurfaceMode::AcceleratedPaint
    }

    fn acquire_frame(
        &mut self,
        ctx: &HostWgpuContext,
    ) -> Result<Option<ImportedTexture>, WeldError> {
        let frame = self.frame_slot.lock().unwrap().frame.take();
        match frame {
            None => Ok(None),
            Some(f) => Ok(Some(WgpuTextureImporter::import(f, ctx)?)),
        }
    }

    fn resize(&mut self, size: PhysicalSize<u32>) {
        self.size = size;
        todo!("cef_browser_host_t::was_resized()")
    }

    fn navigate_to_url(&mut self, _url: &str) {
        todo!("cef_frame_t::load_url()")
    }

    fn navigate_to_string(&mut self, _content: &str, _mime_type: &str) {
        todo!("cef_frame_t::load_string()")
    }

    fn reload(&mut self) {
        todo!("cef_browser_t::reload()")
    }

    fn stop(&mut self) {
        todo!("cef_browser_t::stop_load()")
    }

    fn go_back(&mut self) {
        todo!("cef_browser_t::go_back()")
    }

    fn go_forward(&mut self) {
        todo!("cef_browser_t::go_forward()")
    }

    fn send_mouse_input(&mut self, _event: MouseEvent) {
        todo!("cef_browser_host_t mouse input")
    }

    fn send_keyboard_input(&mut self, _event: KeyEvent) {
        todo!("cef_browser_host_t::send_key_event()")
    }

    fn move_focus(&mut self, _direction: FocusDirection) {
        todo!("cef_browser_host_t::set_focus() + move_focus()")
    }

    fn post_web_message(&mut self, _message: &str) {
        todo!("cef_frame_t::send_process_message()")
    }

    fn poll_web_message(&mut self) -> Option<String> {
        self.events.lock().unwrap().web_messages.pop_front()
    }

    fn poll_navigation_event(&mut self) -> Option<NavigationEvent> {
        self.events.lock().unwrap().nav.pop_front()
    }

    fn execute_script(&mut self, _script: &str, _source_url: &str) {
        todo!("cef_frame_t::execute_java_script()")
    }

    fn open_devtools(&self) {
        todo!("cef_browser_host_t::show_dev_tools()")
    }

    fn browser_id(&self) -> i32 {
        self.browser_id
    }

    fn close(&mut self) {
        todo!("cef_browser_host_t::close_browser()")
    }
}
