// Copyright 2026 Mark Alan Boykin
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0

use crate::native_frame::ImportError;

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum WeldError {
    /// A constructor was called on a build without the `cef-runtime` feature.
    /// Enable the feature in `welding`'s Cargo dependencies.
    #[error("welding feature {0} is required for this operation")]
    FeatureRequired(&'static str),

    /// `cef::load_library` returned 0 (failed to load the CEF framework /
    /// shared library from the specified path).
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
