// Copyright 2026 Mark Alan Boykin
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0

#[path = "blit.rs"]
mod blit;
#[path = "keys.rs"]
mod keys;
#[path = "main.rs"]
mod main_app;
#[path = "probe.rs"]
mod probe;
#[path = "scripted.rs"]
mod scripted;

/// Entry point loaded by CEF's Windows sandbox bootstrap executable.
///
/// # Safety
///
/// This function is called only by CEF's matching `bootstrap.exe`. Its raw
/// instance and sandbox pointers must remain valid for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn RunWinMain(
    instance: cef::sys::HINSTANCE,
    _command_line: *const u8,
    _command_show: i32,
    sandbox_info: *mut u8,
) -> i32 {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: the bootstrap supplied both values and retains ownership.
        unsafe { main_app::run_bootstrap(instance.0.cast(), sandbox_info) }
    }))
    .unwrap_or_else(|_| {
        eprintln!("weld demo: panic escaped the sandboxed browser entry point");
        199
    })
}
