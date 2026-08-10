//! CEF subprocess helper.
//!
//! On macOS CEF does not re-execute the main application binary for its
//! renderer / GPU / utility processes. It launches the helper executables that
//! the bundler stamps into `Contents/Frameworks/<app> Helper*.app`. All five
//! helper bundles run this same binary; CEF tells them apart by the arguments
//! it passes.
//!
//! The only job here is to load the framework relative to the helper's own
//! bundle and hand control to CEF. Anything else, including logging setup,
//! would run once per subprocess.

#[cfg(target_os = "macos")]
fn main() {
    let exe = std::env::current_exe().expect("helper: current_exe failed");

    // `helper: true` resolves the framework up out of
    // `<app>.app/Contents/Frameworks/<app> Helper.app/Contents/MacOS/`.
    let loader = cef::library_loader::LibraryLoader::new(&exe, true);
    if !loader.load() {
        // No logger here, and stderr is the only channel a helper reliably has.
        eprintln!("helper: failed to load the Chromium Embedded Framework");
        std::process::exit(1);
    }

    // Same pin welding's runtime applies. The helper talks to CEF directly
    // rather than through welding, so it has to do this itself; without it the
    // vtable layout the bindings expect need not match libcef's.
    let _ = cef::api_hash(cef::sys::CEF_API_VERSION_LAST, 0);

    let args = cef::args::Args::new();
    let code = cef::execute_process(Some(args.as_main_args()), None, std::ptr::null_mut());
    std::process::exit(code);
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("demo-weld-mac-helper is macOS-only");
    std::process::exit(1);
}
