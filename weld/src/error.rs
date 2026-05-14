use crate::native_frame::ImportError;

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum WeldError {
    /// Scaffold / non-`cef-runtime` path: libloading failed to open the CEF
    /// shared library from the caller-supplied path.
    #[error("CEF library not found at {path}: {source}")]
    LibraryLoad {
        path: String,
        #[source]
        source: libloading::Error,
    },

    /// `cef-runtime` path: `cef::load_library` returned 0 (failed to load the
    /// CEF framework / shared library from the specified path).
    #[cfg(feature = "cef-runtime")]
    #[error("cef::load_library returned failure for path {path}")]
    CefLoadFailed { path: String },

    #[error("CEF initialization failed (code {code})")]
    InitFailed { code: i32 },

    // cef_execute_process returned >= 0: caller must exit with this code.
    // Returned as Err rather than Ok(Some(code)) so the ? operator propagates it
    // without the caller needing to inspect the Ok variant in normal paths.
    #[error("CEF subprocess exit (code {0}); caller must std::process::exit with this value")]
    SubprocessExit(i32),

    /// OnAcceleratedPaint is not being called — GPU support not available in
    /// this CEF build, or windowless_rendering_enabled was not set.
    #[error("CEF accelerated OSR unavailable: {reason}")]
    AcceleratedOsrUnavailable { reason: &'static str },

    #[error("CEF surface creation failed: {0}")]
    SurfaceCreation(String),

    #[error("frame import error: {0}")]
    Import(#[from] ImportError),

    #[error("not supported on this platform: {0}")]
    PlatformUnsupported(&'static str),

    #[error("CEF browser operation failed: {0}")]
    BrowserOp(String),
}
