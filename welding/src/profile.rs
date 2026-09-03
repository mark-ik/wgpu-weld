// Copyright 2026 Mark Alan Boykin
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0

//! Per-producer CEF request contexts.
//!
//! A CEF `Settings::root_cache_path` belongs to the whole process. Passing no
//! request context when a browser is created therefore quietly puts every
//! producer in the global context, even when its public surface configuration
//! claims a separate user-data directory. Creating a context here is the
//! ownership boundary: one surface, one cookie/storage/cache profile.

use crate::{WeldError, runtime::CefRuntime, surface::CefSurfaceConfig};

/// Build the distinct request context owned by one producer.
///
/// An empty CEF cache path is intentionally still passed through a fresh
/// context. It produces an isolated in-memory profile, rather than selecting
/// CEF's process-global context.
pub(crate) fn create(
    runtime: &CefRuntime,
    config: &CefSurfaceConfig,
) -> Result<cef::RequestContext, WeldError> {
    create_with_handler(runtime, config, None)
}

/// Build one context and optionally receive CEF's asynchronous initialization
/// callback. macOS uses the callback to defer browser creation until its host
/// event loop has finished bringing a persistent child profile online.
pub(crate) fn create_with_handler(
    runtime: &CefRuntime,
    config: &CefSurfaceConfig,
    handler: Option<&mut cef::RequestContextHandler>,
) -> Result<cef::RequestContext, WeldError> {
    let mut settings = cef::RequestContextSettings::default();

    if let Some(path) = config.user_data_dir.as_ref() {
        if !path.is_absolute() {
            return Err(WeldError::SurfaceCreation(format!(
                "CefSurfaceConfig::user_data_dir must be absolute: {}",
                path.display()
            )));
        }

        let root = runtime.config().cache_path.as_ref().ok_or_else(|| {
            WeldError::SurfaceCreation(
                "CefRuntimeConfig::cache_path is required when a producer uses user_data_dir; \
                 otherwise CEF selects a shared platform-default root cache"
                    .into(),
            )
        })?;
        if !root.is_absolute() {
            return Err(WeldError::SurfaceCreation(format!(
                "CefRuntimeConfig::cache_path must be absolute for persistent profiles: {}",
                root.display()
            )));
        }
        if !path.starts_with(root) {
            return Err(WeldError::SurfaceCreation(format!(
                "CefSurfaceConfig::user_data_dir ({}) must be inside CefRuntimeConfig::cache_path ({})",
                path.display(),
                root.display(),
            )));
        }

        let path: cef::CefString = path.to_string_lossy().as_ref().into();
        settings.cache_path = path;
        // Persistent contexts should retain session cookies as well as the
        // normal persistent cookie jar. An in-memory context has no disk to
        // retain them on, so leaving the CEF default there is correct.
        settings.persist_session_cookies = 1;
    }

    cef::request_context_create_context(Some(&settings), handler).ok_or_else(|| {
        WeldError::SurfaceCreation("cef_request_context_create_context returned None".into())
    })
}
