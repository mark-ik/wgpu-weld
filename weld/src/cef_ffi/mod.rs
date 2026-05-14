/// CEF C API bindings loaded at runtime via `libloading`.
///
/// CEF is not a system library; it must be distributed alongside the app.
/// Loading it at runtime (rather than at link time) means the `weld` crate
/// compiles without CEF installed. The library is located by the path in
/// [`CefRuntimeConfig::cef_path`][crate::runtime::CefRuntimeConfig].
///
/// All function pointers are resolved once in [`CefFunctions::load`] and then
/// called directly. The `libloading::Library` that backs them is kept alive
/// inside `CefRuntime` for the process lifetime.
pub mod types;

use std::os::raw::c_int;

use libloading::{Library, Symbol};

use crate::{error::WeldError, runtime::CefRuntimeConfig};
use types::{CefMainArgs, CefSettings};

// ── Function pointer type aliases ─────────────────────────────────────────────

type FnExecuteProcess =
    unsafe extern "C" fn(args: *const CefMainArgs, app: *mut c_void, sandbox: *mut c_void)
        -> c_int;

type FnInitialize = unsafe extern "C" fn(
    args: *const CefMainArgs,
    settings: *const CefSettings,
    app: *mut c_void,
    sandbox: *mut c_void,
) -> c_int;

type FnVoid = unsafe extern "C" fn();

type FnCreateBrowser = unsafe extern "C" fn(
    window_info: *const c_void, // *const CefWindowInfo
    client: *mut c_void,        // *mut cef_client_t
    url: *const c_void,         // *const CefString
    settings: *const c_void,    // *const CefBrowserSettings
    extra_info: *mut c_void,
    request_context: *mut c_void,
) -> c_int;

/// All CEF entry points resolved at library-load time.
pub struct CefFunctions {
    // Safety: these function pointers are valid for the lifetime of the
    // `Library` they were resolved from. `CefRuntime` holds the `Arc<Library>`
    // alongside `Arc<CefFunctions>`, guaranteeing this invariant.
    pub execute_process: FnExecuteProcess,
    pub initialize: FnInitialize,
    pub shutdown: FnVoid,
    pub run_message_loop: FnVoid,
    pub quit_message_loop: FnVoid,
    pub do_message_loop_work: FnVoid,
    pub browser_host_create_browser: FnCreateBrowser,
}

impl CefFunctions {
    /// Resolve all required CEF symbols from `lib`.
    ///
    /// Fails if any required symbol is absent — indicates a CEF version
    /// mismatch or an incomplete distribution.
    pub fn load(lib: &Library) -> Result<Self, WeldError> {
        unsafe fn sym<T: Copy>(lib: &Library, name: &[u8]) -> Result<T, WeldError> {
            // Strip the lifetime from the Symbol so we can store the fn pointer
            // independently. Safety: `CefFunctions` is always stored alongside
            // the owning `Library` inside `CefRuntime`.
            let s: Symbol<T> = lib
                .get(name)
                .map_err(|e| WeldError::LibraryLoad { path: "<symbol>".into(), source: e })?;
            Ok(*s)
        }

        unsafe {
            Ok(CefFunctions {
                execute_process: sym(lib, b"cef_execute_process\0")?,
                initialize: sym(lib, b"cef_initialize\0")?,
                shutdown: sym(lib, b"cef_shutdown\0")?,
                run_message_loop: sym(lib, b"cef_run_message_loop\0")?,
                quit_message_loop: sym(lib, b"cef_quit_message_loop\0")?,
                do_message_loop_work: sym(lib, b"cef_do_message_loop_work\0")?,
                browser_host_create_browser: sym(
                    lib,
                    b"cef_browser_host_create_browser\0",
                )?,
            })
        }
    }

    /// Call `cef_execute_process`. Returns the exit code (>= 0) if this
    /// invocation is a CEF subprocess, or -1 for the host process.
    pub unsafe fn execute_process(&self) -> c_int {
        let args = CefMainArgs::for_current_process();
        (self.execute_process)(&args, std::ptr::null_mut(), std::ptr::null_mut())
    }

    /// Call `cef_initialize`. Returns 1 on success, 0 on failure.
    pub unsafe fn initialize(&self, config: &CefRuntimeConfig) -> c_int {
        // Build CefSettings from CefRuntimeConfig.
        // The zero-initialised struct is safe: CEF reads only fields up to
        // `settings.size`, and all booleans default to false/0.
        let mut settings = std::mem::zeroed::<CefSettings>();
        settings.size = std::mem::size_of::<CefSettings>();
        settings.windowless_rendering_enabled = 1;
        if config.single_process {
            settings.command_line_args_disabled = 0; // keep args; we'll add --single-process
        }
        settings.log_severity = config.log_severity as c_int;

        let args = CefMainArgs::for_current_process();
        (self.initialize)(&args, &settings, std::ptr::null_mut(), std::ptr::null_mut())
    }
}
