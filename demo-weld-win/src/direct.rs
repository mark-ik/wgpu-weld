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

fn main() {
    std::process::exit(main_app::run_direct());
}
