/// Minimal CEF C API type definitions.
///
/// CEF's C API uses reference-counted structs with function-pointer vtables.
/// Every CEF object begins with `cef_base_ref_counted_t`. The structs defined
/// here cover only the subset needed for accelerated OSR. They are a temporary
/// bridge until the client/render-handler layer uses generated CEF bindings.
///
/// Layout notes:
/// - All structs use `#[repr(C)]` to match the C ABI.
/// - Function pointers use `extern "C"` calling convention.
/// - `*mut T` fields in vtables are always nullable (CEF uses NULL to indicate
///   "not implemented" for optional callbacks).
#[cfg(not(windows))]
use std::os::raw::c_char;
use std::os::raw::{c_int, c_void};

// ── Base ──────────────────────────────────────────────────────────────────────

#[repr(C)]
pub struct CefBaseRefCounted {
    pub size: usize,
    pub add_ref: Option<unsafe extern "C" fn(this: *mut CefBaseRefCounted)>,
    pub release: Option<unsafe extern "C" fn(this: *mut CefBaseRefCounted) -> c_int>,
    pub has_one_ref: Option<unsafe extern "C" fn(this: *mut CefBaseRefCounted) -> c_int>,
    pub has_at_least_one_ref: Option<unsafe extern "C" fn(this: *mut CefBaseRefCounted) -> c_int>,
}

// ── Strings ───────────────────────────────────────────────────────────────────

#[repr(C)]
pub struct CefStringUtf16 {
    pub str_: *mut u16,
    pub length: usize,
    pub dtor: Option<unsafe extern "C" fn(str_: *mut u16)>,
}

/// `cef_string_t` is `cef_string_utf16_t` on all platforms.
pub type CefString = CefStringUtf16;

// ── Geometry ──────────────────────────────────────────────────────────────────

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct CefRect {
    pub x: c_int,
    pub y: c_int,
    pub width: c_int,
    pub height: c_int,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct CefSize {
    pub width: c_int,
    pub height: c_int,
}

// ── Settings ──────────────────────────────────────────────────────────────────

/// `cef_settings_t` — subset of fields used by weld.
/// The full struct has ~50 fields; unused ones are zero-initialised.
#[repr(C)]
pub struct CefSettings {
    pub size: usize,
    pub no_sandbox: c_int,
    pub browser_subprocess_path: CefString,
    pub framework_dir_path: CefString,
    pub main_bundle_path: CefString,
    pub multi_threaded_message_loop: c_int,
    pub external_message_pump: c_int,
    pub windowless_rendering_enabled: c_int,
    pub command_line_args_disabled: c_int,
    pub cache_path: CefString,
    pub root_cache_path: CefString,
    pub persist_session_cookies: c_int,
    pub user_agent: CefString,
    pub user_agent_product: CefString,
    pub locale: CefString,
    pub log_file: CefString,
    pub log_severity: c_int, // cef_log_severity_t
    pub log_items: c_int,
    pub javascript_flags: CefString,
    pub resources_dir_path: CefString,
    pub locales_dir_path: CefString,
    pub remote_debugging_port: c_int,
    pub uncaught_exception_stack_size: c_int,
    pub background_color: u32,
    pub accept_language_list: CefString,
    pub cookieable_schemes_list: CefString,
    pub cookieable_schemes_exclude_defaults: c_int,
    pub chrome_policy_id: CefString,
    pub chrome_app_icon_id: c_int,
    pub disable_signal_handlers: c_int,
    pub use_views_default_popup: c_int,
}

/// `cef_browser_settings_t` — subset used by weld.
#[repr(C)]
pub struct CefBrowserSettings {
    pub size: usize,
    pub windowless_frame_rate: c_int,
    // ... remaining fields zero-initialised
    _pad: [u8; 256],
}

// ── Window info ───────────────────────────────────────────────────────────────

/// `cef_window_info_t` — platform-specific.
/// For OSR (windowless) mode the HWND / NSView / X11 window fields are unused;
/// `windowless_rendering_enabled` is the key field.
#[repr(C)]
pub struct CefWindowInfo {
    pub size: usize,
    #[cfg(windows)]
    pub ex_style: u32,
    pub window_name: CefString,
    #[cfg(windows)]
    pub style: u32,
    pub bounds: CefRect,
    #[cfg(windows)]
    pub parent_window: *mut c_void, // HWND
    #[cfg(windows)]
    pub menu: *mut c_void, // HMENU
    #[cfg(target_os = "linux")]
    pub parent_window: std::os::raw::c_ulong,
    #[cfg(target_os = "macos")]
    pub hidden: c_int,
    #[cfg(target_os = "macos")]
    pub parent_view: *mut c_void,
    pub windowless_rendering_enabled: c_int,
    pub shared_texture_enabled: c_int,
    pub external_begin_frame_enabled: c_int,
    #[cfg(windows)]
    pub window: *mut c_void, // HWND (output)
    #[cfg(target_os = "linux")]
    pub window: std::os::raw::c_ulong,
    #[cfg(target_os = "macos")]
    pub view: *mut c_void,
    pub runtime_style: c_int,
}

// ── Main args ─────────────────────────────────────────────────────────────────

#[repr(C)]
pub struct CefMainArgs {
    #[cfg(windows)]
    pub instance: *mut c_void, // HINSTANCE
    #[cfg(not(windows))]
    pub argc: c_int,
    #[cfg(not(windows))]
    pub argv: *mut *mut c_char,
}

impl CefMainArgs {
    #[cfg(windows)]
    pub fn for_current_process() -> Self {
        unsafe extern "system" {
            fn GetModuleHandleW(name: *const u16) -> *mut c_void;
        }
        CefMainArgs {
            instance: unsafe { GetModuleHandleW(std::ptr::null()) },
        }
    }

    #[cfg(not(windows))]
    pub fn for_current_process() -> Self {
        // argc/argv sourced from std::env::args_os at call site
        CefMainArgs {
            argc: 0,
            argv: std::ptr::null_mut(),
        }
    }
}

// ── Accelerated paint ─────────────────────────────────────────────────────────

/// The payload of `OnAcceleratedPaint`. Platform-specific texture handle.
///
/// # Handle lifetime (CRITICAL)
///
/// On Windows, `shared_texture_handle` is a Win32 `HANDLE` that is valid
/// **only for the duration of the `OnAcceleratedPaint` callback**. It must be
/// imported or duplicated before the callback returns.
///
/// On macOS, `shared_texture_io_surface` is an `IOSurfaceRef` (a `*mut c_void`
/// whose underlying object is ref-counted). `weld` retains it before the
/// callback returns and releases it after import.
#[repr(C)]
pub struct CefAcceleratedPaintInfo {
    pub size: usize,
    #[cfg(windows)]
    pub shared_texture_handle: *mut c_void,
    #[cfg(target_os = "macos")]
    pub shared_texture_io_surface: *mut c_void,
    #[cfg(target_os = "linux")]
    pub planes: [CefAcceleratedPaintNativePixmapPlaneInfo; 4],
    #[cfg(target_os = "linux")]
    pub plane_count: c_int,
    #[cfg(target_os = "linux")]
    pub modifier: u64,
    /// Pixel format of the texture (`BGRA8` on all current platforms).
    pub format: c_int, // cef_color_type_t
    pub extra: CefAcceleratedPaintInfoCommon,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct CefAcceleratedPaintNativePixmapPlaneInfo {
    pub stride: u32,
    pub offset: u64,
    pub size: u64,
    pub fd: c_int,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct CefAcceleratedPaintInfoCommon {
    pub size: usize,
    pub timestamp: u64,
    pub coded_size: CefSize,
    pub visible_rect: CefRect,
    pub content_rect: CefRect,
    pub source_size: CefSize,
    pub capture_update_rect: CefRect,
    pub region_capture_rect: CefRect,
    pub capture_counter: u64,
    pub has_capture_update_rect: u8,
    pub has_region_capture_rect: u8,
    pub has_source_size: u8,
    pub has_capture_counter: u8,
}
