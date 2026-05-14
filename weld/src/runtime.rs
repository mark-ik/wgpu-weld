use std::{path::PathBuf, sync::Arc};

use libloading::Library;

use crate::{cef_ffi::CefFunctions, error::WeldError};

/// CEF's global runtime. Must be initialised once per process before any other
/// CEF call; dropping this value calls `cef_shutdown`.
///
/// # Subprocess Tax — Read This First
///
/// CEF spawns renderer, GPU, and utility processes by re-executing the host
/// binary. **You must call [`CefRuntime::execute_process_from`] at the very top
/// of `main()`, before any other initialisation,** and immediately
/// `std::process::exit` if it returns `Some(code)`:
///
/// ```no_run
/// fn main() {
///     let cef_path = std::env::var("CEF_PATH").expect("CEF_PATH required");
///     if let Some(code) = weld::CefRuntime::execute_process_from(cef_path.as_ref())
///         .expect("failed to probe CEF subprocess role")
///     {
///         std::process::exit(code);
///     }
///     // now safe to initialise the runtime and create browsers
/// }
/// ```
///
/// Forgetting this call causes the subprocess pool to starve. The GPU process
/// never starts; `OnAcceleratedPaint` callbacks never fire and the OSR surface
/// stays blank.
pub struct CefRuntime {
    // Library must outlive all function pointers derived from it.
    _lib: Arc<Library>,
    fns: Arc<CefFunctions>,
    config: CefRuntimeConfig,
}

impl CefRuntime {
    /// Load `libcef` from `cef_path` and call `cef_execute_process`.
    ///
    /// Returns `Ok(Some(exit_code))` if this invocation is a CEF subprocess —
    /// the caller must `std::process::exit(exit_code)` and do nothing else.
    /// Returns `Ok(None)` for the host (main) process.
    ///
    /// This is the only call that is safe before [`CefRuntime::initialize`].
    pub fn execute_process_from(cef_path: &std::path::Path) -> Result<Option<i32>, WeldError> {
        let lib = unsafe {
            let lib_name = cef_lib_name();
            Library::new(cef_path.join(lib_name)).map_err(|e| WeldError::LibraryLoad {
                path: cef_path.display().to_string(),
                source: e,
            })?
        };
        let fns = CefFunctions::load(&lib)?;
        let code = unsafe { fns.execute_process() };
        if code >= 0 {
            Ok(Some(code))
        } else {
            Ok(None)
        }
    }

    /// Load CEF from `config.cef_path` and initialise the runtime.
    ///
    /// Only one `CefRuntime` may exist at a time. Calling this a second time
    /// before dropping the first is undefined behaviour at the CEF level.
    pub fn initialize(config: CefRuntimeConfig) -> Result<Self, WeldError> {
        let lib = Arc::new(unsafe {
            let lib_name = cef_lib_name();
            Library::new(config.cef_path.join(lib_name)).map_err(|e| WeldError::LibraryLoad {
                path: config.cef_path.display().to_string(),
                source: e,
            })?
        });
        let fns = Arc::new(CefFunctions::load(&lib)?);
        let code = unsafe { fns.initialize(&config) };
        if code == 0 {
            return Err(WeldError::InitFailed { code });
        }
        Ok(CefRuntime { _lib: lib, fns, config })
    }

    /// Run CEF's message loop until `cef_quit_message_loop` is called.
    /// Blocks the calling thread. Use when CEF owns the main-thread event loop.
    pub fn run_message_loop(&self) {
        unsafe { (self.fns.run_message_loop)() }
    }

    /// Pump CEF's message loop once. Call on every host event-loop tick when
    /// integrating CEF into an existing loop (e.g. winit `run`).
    pub fn do_message_loop_work(&self) {
        unsafe { (self.fns.do_message_loop_work)() }
    }

    pub fn config(&self) -> &CefRuntimeConfig {
        &self.config
    }

    pub(crate) fn fns(&self) -> Arc<CefFunctions> {
        self.fns.clone()
    }
}

impl Drop for CefRuntime {
    fn drop(&mut self) {
        unsafe { (self.fns.shutdown)() }
    }
}

/// Configuration passed to [`CefRuntime::initialize`].
#[derive(Clone, Debug)]
pub struct CefRuntimeConfig {
    /// Directory containing the CEF binary distribution.
    ///
    /// - Windows: the folder with `libcef.dll`, `icudtl.dat`, locale files, etc.
    /// - macOS: the folder containing `Chromium Embedded Framework.framework`.
    /// - Linux: the folder with `libcef.so`, `icudtl.dat`, locale files, etc.
    pub cef_path: PathBuf,

    /// Path to the subprocess helper executable. When `None` the current binary
    /// is re-used (the standard pattern; requires calling
    /// [`CefRuntime::execute_process_from`] at the start of `main()`).
    pub browser_subprocess_path: Option<PathBuf>,

    /// Persistent cache directory (cookies, storage, IndexedDB, etc.).
    /// `None` = in-memory / temporary.
    pub cache_path: Option<PathBuf>,

    /// **Not for production.** Run all CEF logic in a single process; skip
    /// subprocess spawning. Useful during development and for integration tests.
    pub single_process: bool,

    pub log_severity: CefLogSeverity,
}

impl CefRuntimeConfig {
    pub fn new(cef_path: impl Into<PathBuf>) -> Self {
        CefRuntimeConfig {
            cef_path: cef_path.into(),
            browser_subprocess_path: None,
            cache_path: None,
            single_process: false,
            log_severity: CefLogSeverity::Default,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CefLogSeverity {
    #[default]
    Default,
    Verbose,
    Info,
    Warning,
    Error,
    Fatal,
    Disable,
}

fn cef_lib_name() -> &'static str {
    #[cfg(windows)]
    return "libcef.dll";
    #[cfg(target_os = "macos")]
    return "Chromium Embedded Framework.framework/Chromium Embedded Framework";
    #[cfg(target_os = "linux")]
    return "libcef.so";
}
