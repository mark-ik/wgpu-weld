// Copyright 2026 Mark Alan Boykin
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0

//! Build the sandboxed Windows CEF bundle.

#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::{path::PathBuf, process::Command};

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_dir = manifest_dir
        .parent()
        .expect("demo package must be inside the weld workspace");
    std::env::set_current_dir(workspace_dir)?;

    let output = PathBuf::from(
        std::env::args()
            .nth(1)
            .unwrap_or_else(|| "target/bundle/demo-weld-win".to_owned()),
    );
    std::fs::create_dir_all(&output)?;

    let release = std::env::args().any(|arg| arg == "--release");
    let mut build = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    build.args(["build", "--locked", "--package", "demo-weld-win", "--lib"]);
    if release {
        build.arg("--release");
    }
    if !build.status()?.success() {
        return Err("sandbox client DLL build failed".into());
    }

    let target_root = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace_dir.join("target"));
    let profile_dir = target_root.join(if release { "release" } else { "debug" });
    let stage =
        std::env::temp_dir().join(format!("demo-weld-win-bundle-stage-{}", std::process::id()));
    std::fs::create_dir_all(&stage)?;
    std::fs::copy(
        profile_dir.join("demo_weld_win.dll"),
        stage.join("demo-weld-win.dll"),
    )?;
    std::fs::copy(
        profile_dir.join("demo_weld_win.pdb"),
        stage.join("demo-weld-win.pdb"),
    )?;

    let executable = cef::build_util::win::bundle(&output, &stage, "demo-weld-win")?;
    std::fs::remove_dir_all(&stage)?;
    println!("bundled: {}", executable.display());
    Ok(())
}

#[cfg(not(windows))]
fn main() {
    eprintln!("bundle-demo-weld-win is Windows-only");
    std::process::exit(1);
}
