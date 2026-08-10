use std::path::PathBuf;

use crate::error::WeldError;

// ── Shared config / log-severity (always available) ──────────────────────────

/// Configuration passed to [`CefRuntime::initialize`].
///
/// # Subprocess Tax — Read This First
///
/// CEF re-executes this binary as its renderer/GPU/utility subprocesses. You
/// must call [`CefRuntime::execute_process_from`] **as the first line of
/// `main()`** and `std::process::exit` immediately if it returns `Some(code)`:
///
/// ```no_run
/// fn main() {
///     let cef_path = std::env::var("CEF_PATH").expect("CEF_PATH required");
///     if let Some(code) = welding::CefRuntime::execute_process_from(cef_path.as_ref())
///         .expect("failed to probe CEF subprocess role")
///     {
///         std::process::exit(code);
///     }
///     // now safe to call CefRuntime::initialize and create browsers
/// }
/// ```
#[derive(Clone, Debug)]
pub struct CefRuntimeConfig {
    /// Directory containing the CEF binary distribution.
    ///
    /// - Windows: folder with `libcef.dll`, `icudtl.dat`, locale files, etc.
    ///   Under `cef-runtime` this is added to the DLL search path via
    ///   `SetDllDirectory` when CEF is not adjacent to the executable.
    /// - macOS: folder containing `Chromium Embedded Framework.framework`.
    ///   Under `cef-runtime` this is passed to `cef::load_library`.
    /// - Linux: folder with `libcef.so`, `icudtl.dat`, locale files, etc.
    pub cef_path: PathBuf,

    /// Path to the subprocess helper executable. `None` = re-use this binary
    /// (requires calling [`CefRuntime::execute_process_from`] at `main()` start).
    pub browser_subprocess_path: Option<PathBuf>,

    /// Persistent cache directory. `None` = in-memory / temporary.
    pub cache_path: Option<PathBuf>,

    /// **Not for production.** Single-process mode; skips subprocess spawning.
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

// ── cef-runtime: cef crate–backed implementation ─────────────────────────────

#[cfg(feature = "cef-runtime")]
mod cef_backed {
    use super::*;
    use cef::*;
    use cef::args::Args;

    // Minimal no-op App impl: CEF requires an App on all initialize paths; this
    // satisfies that without requiring callers to depend on the cef crate.
    #[derive(Clone)]
    struct WeldApp;

    cef::wrap_app! {
        struct WeldAppWrapper {
            app: WeldApp,
        }
        impl App {}
    }

    impl WeldAppWrapper {
        fn build() -> cef::App {
            Self::new(WeldApp)
        }
    }

    /// CEF global runtime under `cef-runtime`. The `cef` crate (via `cef-dll-sys`)
    /// owns the process-global CEF state; this struct is a thin RAII drop-guard
    /// that calls `cef::shutdown` when dropped.
    pub struct CefRuntime {
        pub(super) config: CefRuntimeConfig,
    }

    impl CefRuntime {
        /// Load libcef and call `cef_execute_process`. Must be the very first
        /// CEF call in `main()`.
        ///
        /// Returns `Ok(Some(exit_code))` for CEF subprocesses — the caller must
        /// `std::process::exit(exit_code)`. Returns `Ok(None)` for the browser
        /// (host) process.
        pub fn execute_process_from(
            cef_path: &std::path::Path,
        ) -> Result<Option<i32>, WeldError> {
            maybe_load_library(cef_path)?;
            pin_cef_api_version();
            let args = Args::new();
            let mut app = WeldAppWrapper::build();
            let code = cef::execute_process(
                Some(args.as_main_args()),
                Some(&mut app),
                std::ptr::null_mut(),
            );
            if code >= 0 { Ok(Some(code)) } else { Ok(None) }
        }

        /// Initialise CEF. Call after [`Self::execute_process_from`] confirms
        /// this is the browser process.
        pub fn initialize(config: CefRuntimeConfig) -> Result<Self, WeldError> {
            maybe_load_library(&config.cef_path)?;
            pin_cef_api_version();
            let args = Args::new();
            let mut app = WeldAppWrapper::build();
            let settings = build_settings(&config);
            let code = cef::initialize(
                Some(args.as_main_args()),
                Some(&settings),
                Some(&mut app),
                std::ptr::null_mut(),
            );
            if code == 0 {
                return Err(WeldError::InitFailed { code });
            }
            Ok(CefRuntime { config })
        }

        /// Block on CEF's message loop. Use when CEF owns the main thread.
        pub fn run_message_loop(&self) {
            cef::run_message_loop();
        }

        /// Pump CEF's message loop once on platforms where the host owns it.
        ///
        /// Windows uses CEF's dedicated message-loop thread so this is a no-op.
        pub fn do_message_loop_work(&self) {
            #[cfg(not(target_os = "windows"))]
            cef::do_message_loop_work();
        }

        pub fn config(&self) -> &CefRuntimeConfig {
            &self.config
        }
    }

    impl Drop for CefRuntime {
        fn drop(&mut self) {
            cef::shutdown();
        }
    }

    /// Pin the CEF API version for this process.
    ///
    /// CEF 148 introduced API versioning: each call that crosses the libcef
    /// boundary checks the host has pinned a version. Without this, libcef
    /// aborts with "CefApp_0_CToCpp called with invalid version -1" the first
    /// time the App vtable is dispatched. The pin lasts for the process and
    /// only the first call has effect, so it's safe to call at the top of
    /// both `execute_process_from` (subprocess path) and `initialize` (host
    /// path).
    ///
    /// We pin to `CEF_API_VERSION_LAST` — the latest version the cef-dll-sys
    /// bindings know about, which by construction matches the libcef.so we
    /// link against. Cross-platform: required equally on Linux, Windows, and
    /// macOS as of CEF 148.
    fn pin_cef_api_version() {
        let _ = cef::api_hash(cef::sys::CEF_API_VERSION_LAST, 0);
    }

    fn build_settings(config: &CefRuntimeConfig) -> cef::Settings {
        let mut s = cef::Settings {
            windowless_rendering_enabled: 1,
            // Windows uses CEF's supported dedicated UI thread. Other platforms
            // continue to integrate CefDoMessageLoopWork into the host loop.
            multi_threaded_message_loop: cfg!(target_os = "windows") as _,
            external_message_pump: cfg!(not(target_os = "windows")) as _,
            // We pass null sandbox_info to CEF initialize/execute_process;
            // disable sandbox mode so subprocesses (GPU/renderer/utility)
            // can start reliably across dev environments.
            no_sandbox: 1,
            log_severity: match config.log_severity {
                CefLogSeverity::Default => LogSeverity::DEFAULT,
                CefLogSeverity::Verbose => LogSeverity::VERBOSE,
                CefLogSeverity::Info    => LogSeverity::INFO,
                CefLogSeverity::Warning => LogSeverity::WARNING,
                CefLogSeverity::Error   => LogSeverity::ERROR,
                CefLogSeverity::Fatal   => LogSeverity::FATAL,
                CefLogSeverity::Disable => LogSeverity::DISABLE,
            },
            ..Default::default()
        };
        // root_cache_path: CEF uses this directory as the parent of all
        // browser cache/session data. Without it, CEF logs a warning about
        // "default value may lead to unintended process singleton behavior"
        // and may share state unexpectedly across welding-using applications.
        if let Some(cache_path) = config.cache_path.as_ref() {
            let path_str = cache_path.to_string_lossy();
            s.root_cache_path = (&*path_str).into();
        }
        if let Some(subprocess_path) = config.browser_subprocess_path.as_ref() {
            let path_str = subprocess_path.to_string_lossy();
            s.browser_subprocess_path = (&*path_str).into();
        }
        s
    }

    /// Platform-specific library loading:
    /// - macOS: `cef::load_library` must be called before any other CEF API.
    /// - Windows: add `cef_path` to the DLL search path so `libcef.dll` is found
    ///   even when not adjacent to the executable.
    /// - Linux: `libcef.so` is expected to be in `LD_LIBRARY_PATH`; no action.
    fn maybe_load_library(cef_path: &std::path::Path) -> Result<(), WeldError> {
        #[cfg(target_os = "macos")]
        {
            use std::os::unix::ffi::OsStrExt;

            const FRAMEWORK: &str =
                "Chromium Embedded Framework.framework/Chromium Embedded Framework";
            let lib_path = cef_path.join(FRAMEWORK);
            // `cef_load_library` takes a plain C string path, not a CefString.
            let c_path = std::ffi::CString::new(lib_path.as_os_str().as_bytes()).map_err(|_| {
                WeldError::CefLoadFailed {
                    path: lib_path.display().to_string(),
                }
            })?;
            let ok = unsafe { cef::load_library(Some(&*c_path.as_ptr().cast())) };
            if ok != 1 {
                return Err(WeldError::CefLoadFailed {
                    path: lib_path.display().to_string(),
                });
            }
        }
        #[cfg(target_os = "windows")]
        {
            use windows::Win32::System::LibraryLoader::SetDllDirectoryW;
            use windows::core::HSTRING;
            let wide = HSTRING::from(cef_path.as_os_str());
            // Ignore failure: if libcef.dll is already in the search path this is
            // a no-op.
            unsafe { let _ = SetDllDirectoryW(&wide); }
        }
        let _ = cef_path;
        Ok(())
    }
}

#[cfg(feature = "cef-runtime")]
pub use cef_backed::CefRuntime;

// ── stub: no `cef-runtime` feature ────────────────────────────────────────────
//
// Without the `cef-runtime` feature, the crate still compiles so downstream
// code can reference its types, but the runtime returns errors on every call.
// (The previous libloading-backed scaffold was removed — it was unused and
// duplicated CEF type definitions that drift across CEF versions.)

#[cfg(not(feature = "cef-runtime"))]
mod stub {
    use super::*;

    /// CEF global runtime stub. Construct only via [`initialize`]; every
    /// method returns [`WeldError::FeatureRequired`].
    pub struct CefRuntime {
        pub(super) config: CefRuntimeConfig,
    }

    impl CefRuntime {
        pub fn execute_process_from(
            _cef_path: &std::path::Path,
        ) -> Result<Option<i32>, WeldError> {
            Err(WeldError::FeatureRequired("cef-runtime"))
        }

        pub fn initialize(_config: CefRuntimeConfig) -> Result<Self, WeldError> {
            Err(WeldError::FeatureRequired("cef-runtime"))
        }

        pub fn run_message_loop(&self) {}
        pub fn do_message_loop_work(&self) {}

        pub fn config(&self) -> &CefRuntimeConfig {
            &self.config
        }
    }
}

#[cfg(not(feature = "cef-runtime"))]
pub use stub::CefRuntime;
