//! Builds `demo-weld-mac.app`.
//!
//! The `cef` crate ships an equivalent `bundle-cef-app` binary, but a
//! dependency's binaries cannot be selected with `cargo run -p`, so this calls
//! the same library entry point instead. It builds both bins, lays out the
//! `.app`, copies the framework in, and stamps the helper into all five Helper
//! bundles.
//!
//! Run it from this crate's directory so the nested `cargo build --bin` calls
//! resolve against this package:
//!
//! ```text
//! cd demo-weld-mac && cargo run --bin bundle-demo-weld-mac
//! ```

#[cfg(target_os = "macos")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use cef::build_util::mac::{BundleInfo, build_bundle};

    // cef's bundler launches nested `cargo build --bin ...` commands. Resolve
    // those against this package even when the outer command starts at the
    // workspace root, as CI and most users naturally do.
    std::env::set_current_dir(env!("CARGO_MANIFEST_DIR"))?;

    let output = std::path::PathBuf::from(
        std::env::args()
            .nth(1)
            .unwrap_or_else(|| "../target/bundle".to_owned()),
    );
    std::fs::create_dir_all(&output)?;

    let bundle_path = build_bundle(
        &output,
        "demo-weld-mac",
        BundleInfo {
            name: "demo-weld-mac".to_owned(),
            identifier: "made.merely.wgpu-weld.demo".to_owned(),
            display_name: "welding demo".to_owned(),
            development_region: "English".to_owned(),
            version: semver::Version::new(0, 1, 0),
        },
    )?;

    println!("bundled: {}", bundle_path.display());
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("bundle-demo-weld-mac is macOS-only");
    std::process::exit(1);
}
