/// Windows CEF producer: accelerated OSR via `OnAcceleratedPaint`.
///
/// # Handle lifetime (CRITICAL)
///
/// `CefAcceleratedPaintInfo::shared_texture_handle` is a Win32 `HANDLE` that is
/// valid **only for the duration of the `OnAcceleratedPaint` callback**. This
/// module calls `DuplicateHandle` inside the callback to produce an owned copy
/// that survives until [`WindowsCefProducer::acquire_frame`] imports it into
/// wgpu via the D3D11 open-shared → D3D12 path.
///
/// # CEF vtable wiring (not yet implemented)
///
/// `WindowsCefProducer::new` needs to allocate a `cef_client_t` +
/// `cef_render_handler_t` struct pair whose vtable function pointers close over
/// an `Arc<FrameSlot>` via `Box::into_raw` / user-data pointer. The stubs in
/// this file represent the correct data-flow shape; the raw vtable allocation is
/// left as `todo!()` until CEF integration is active.
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use dpi::PhysicalSize;

use crate::{
    cef_ffi::CefFunctions,
    error::WeldError,
    native_frame::{Dx12SharedTexture, HostWgpuContext, ImportedTexture, NativeFrame, WgpuTextureImporter},
    runtime::CefRuntime,
    surface::{
        CefSurfaceConfig, CefSurfaceMode, CefSurfaceProducer,
        FocusDirection, KeyEvent, MouseEvent, NavigationEvent,
    },
};

// ── Public config ─────────────────────────────────────────────────────────────

pub struct WindowsCefConfig {
    pub surface: CefSurfaceConfig,
}

impl Default for WindowsCefConfig {
    fn default() -> Self {
        WindowsCefConfig { surface: CefSurfaceConfig::default() }
    }
}

// ── Shared callback state ─────────────────────────────────────────────────────

/// Written by the `OnAcceleratedPaint` callback thread; read by `acquire_frame`
/// on the host thread. The `DuplicateHandle`'d `HANDLE` inside
/// `Dx12SharedTexture` is valid until `acquire_frame` imports it.
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

pub struct WindowsCefProducer {
    fns: Arc<CefFunctions>,
    /// CEF browser identifier assigned by `OnAfterCreated`.
    browser_id: i32,
    frame_slot: Arc<Mutex<FrameSlot>>,
    events: Arc<Mutex<EventQueues>>,
    size: PhysicalSize<u32>,
}

impl WindowsCefProducer {
    /// Create a CEF browser in OSR (windowless + shared-texture) mode and
    /// return a producer bound to it.
    ///
    /// The browser is created asynchronously; `acquire_frame` returns `Ok(None)`
    /// until `OnAcceleratedPaint` fires for the first time.
    ///
    /// # Handle duplication in `OnAcceleratedPaint`
    ///
    /// The vtable wiring (not yet implemented) must do the following inside the
    /// callback before storing the frame:
    ///
    /// ```text
    /// DuplicateHandle(
    ///     GetCurrentProcess(),
    ///     info.shared_texture_handle,   // source: transient Win32 HANDLE
    ///     GetCurrentProcess(),
    ///     &mut dup,                     // destination: owned HANDLE
    ///     0,
    ///     FALSE,
    ///     DUPLICATE_SAME_ACCESS,
    /// );
    /// *frame_slot.lock() = Some(NativeFrame::Dx12SharedTexture(
    ///     Dx12SharedTexture { handle: dup, width, height }
    /// ));
    /// ```
    pub fn new(runtime: &CefRuntime, config: WindowsCefConfig) -> Result<Self, WeldError> {
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
        // TODO: allocate cef_client_t + cef_render_handler_t vtable structs;
        //       register Arc<FrameSlot> via Box::into_raw as user-data;
        //       build CefWindowInfo (windowless_rendering_enabled=1,
        //       shared_texture_enabled=1);
        //       call fns.browser_host_create_browser.
        todo!("wire cef_client_t + cef_render_handler_t vtables; create browser")
    }
}

impl CefSurfaceProducer for WindowsCefProducer {
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
        // TODO: cef_browser_host_t::was_resized() — triggers a new OnAcceleratedPaint
        todo!("call browser_host->was_resized()")
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
        // cef_browser_host_t::send_mouse_move_event / send_mouse_click_event /
        // send_mouse_wheel_event — translate MouseEvent → CefMouseEvent + type.
        todo!("cef_browser_host_t mouse input")
    }

    fn send_keyboard_input(&mut self, _event: KeyEvent) {
        // cef_browser_host_t::send_key_event — translate KeyEvent → CefKeyEvent.
        todo!("cef_browser_host_t::send_key_event()")
    }

    fn move_focus(&mut self, _direction: FocusDirection) {
        todo!("cef_browser_host_t::set_focus() + move_focus()")
    }

    fn post_web_message(&mut self, _message: &str) {
        // cef_frame_t::send_process_message — build a CefProcessMessage with the
        // string payload; CEF delivers it to the renderer-process OnProcessMessageReceived.
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
        // cef_browser_host_t::close_browser(force_close=1).
        // Sets browser_id to 0; further calls are no-ops.
        todo!("cef_browser_host_t::close_browser()")
    }
}
