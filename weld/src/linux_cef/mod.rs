/// Linux CEF producer: accelerated OSR via DMABUF + Vulkan external memory.
///
/// # Status
///
/// The CEF `OnAcceleratedPaint` / DMABUF path for Linux is not yet stabilised
/// in the CEF public API (as of CEF 130). This module is a structural scaffold
/// matching the Windows and macOS producers; all methods are `todo!()` pending
/// the CEF Linux GPU path becoming official.
///
/// # DMABUF fd lifetime
///
/// When the CEF Linux path lands, `CefAcceleratedPaintInfo::shared_texture_handle`
/// will be a DMABUF file descriptor. Unlike Windows (HANDLE duplication) and
/// macOS (IOSurface retain), a DMABUF fd must be `dup(2)`'d inside the callback
/// if it needs to outlive the callback's return. Alternatively, import can be
/// done synchronously within the callback before returning.
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use dpi::PhysicalSize;

use crate::{
    cef_ffi::CefFunctions,
    error::WeldError,
    native_frame::{HostWgpuContext, ImportedTexture, NativeFrame, WgpuTextureImporter},
    runtime::CefRuntime,
    surface::{
        CefSurfaceConfig, CefSurfaceMode, CefSurfaceProducer,
        FocusDirection, KeyEvent, MouseEvent, NavigationEvent,
    },
};

// ── Public config ─────────────────────────────────────────────────────────────

pub struct LinuxCefConfig {
    pub surface: CefSurfaceConfig,
}

impl Default for LinuxCefConfig {
    fn default() -> Self {
        LinuxCefConfig { surface: CefSurfaceConfig::default() }
    }
}

// ── Shared callback state ─────────────────────────────────────────────────────

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

pub struct LinuxCefProducer {
    fns: Arc<CefFunctions>,
    browser_id: i32,
    frame_slot: Arc<Mutex<FrameSlot>>,
    events: Arc<Mutex<EventQueues>>,
    size: PhysicalSize<u32>,
}

impl LinuxCefProducer {
    pub fn new(runtime: &CefRuntime, config: LinuxCefConfig) -> Result<Self, WeldError> {
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
        todo!("Linux CEF accelerated OSR path not yet stabilised upstream")
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
