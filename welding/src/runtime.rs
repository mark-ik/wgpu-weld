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
///     if let Some(code) = weld::CefRuntime::execute_process_from(cef_path.as_ref())
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

        /// Pump CEF's message loop once. Call on every host event-loop tick.
        pub fn do_message_loop_work(&self) {
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

    fn build_settings(config: &CefRuntimeConfig) -> cef::Settings {
        let s = cef::Settings {
            windowless_rendering_enabled: 1,
            external_message_pump: 1,
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
            const FRAMEWORK: &str =
                "Chromium Embedded Framework.framework/Chromium Embedded Framework";
            let lib_path = cef_path.join(FRAMEWORK);
            let path_str = lib_path.to_string_lossy();
            let cef_str: cef::CefString = (&*path_str).into();
            let ok = unsafe { cef::load_library(Some(&cef_str)) };
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

// ── scaffold: libloading-backed implementation ────────────────────────────────

#[cfg(not(feature = "cef-runtime"))]
mod scaffold {
    use super::*;
    use std::sync::Arc;
    use libloading::Library;
    use crate::cef_ffi::CefFunctions;

    /// CEF global runtime (scaffold — no `cef-runtime` feature). Uses
    /// `libloading` to resolve CEF symbols at runtime from `CefRuntimeConfig::cef_path`.
    pub struct CefRuntime {
        _lib: Arc<Library>,
        fns: Arc<CefFunctions>,
        pub(super) config: CefRuntimeConfig,
    }

    impl CefRuntime {
        pub fn execute_process_from(
            cef_path: &std::path::Path,
        ) -> Result<Option<i32>, WeldError> {
            let lib = unsafe {
                Library::new(cef_path.join(cef_lib_name())).map_err(|e| {
                    WeldError::LibraryLoad { path: cef_path.display().to_string(), source: e }
                })?
            };
            let fns = CefFunctions::load(&lib)?;
            let code = unsafe { fns.execute_process() };
            if code >= 0 { Ok(Some(code)) } else { Ok(None) }
        }

        pub fn initialize(config: CefRuntimeConfig) -> Result<Self, WeldError> {
            let lib = Arc::new(unsafe {
                Library::new(config.cef_path.join(cef_lib_name())).map_err(|e| {
                    WeldError::LibraryLoad {
                        path: config.cef_path.display().to_string(),
                        source: e,
                    }
                })?
            });
            let fns = Arc::new(CefFunctions::load(&lib)?);
            let code = unsafe { fns.initialize(&config) };
            if code == 0 {
                return Err(WeldError::InitFailed { code });
            }
            Ok(CefRuntime { _lib: lib, fns, config })
        }

        pub fn run_message_loop(&self) {
            unsafe { (self.fns.run_message_loop)() }
        }

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

    fn cef_lib_name() -> &'static str {
        #[cfg(windows)]
        return "libcef.dll";
        #[cfg(target_os = "macos")]
        return "Chromium Embedded Framework.framework/Chromium Embedded Framework";
        #[cfg(target_os = "linux")]
        return "libcef.so";
    }
}

#[cfg(not(feature = "cef-runtime"))]
pub use scaffold::CefRuntime;
