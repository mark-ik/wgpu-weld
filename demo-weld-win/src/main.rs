//! Windows demo: CEF accelerated OSR → wgpu surface via `weld`.
//!
//! # Running
//!
//! ```text
//! set CEF_PATH=C:\path\to\cef_binary_130.x.x_windows64
//! cargo run -p demo-weld-win
//! ```
//!
//! CEF must be built with `--enable-shared-texture` and
//! `--enable-osr`. Use the standard "Release" distribution.
//!
//! # Subprocess tax
//!
//! `CefRuntime::execute_process_from` **must be the very first call in
//! `main()`**. CEF re-invokes this binary as its renderer / GPU / utility
//! subprocess; those invocations must call `execute_process_from` and exit
//! immediately. Any initialisation before that call (winit, wgpu, etc.) will
//! execute in the subprocess context and produce undefined behaviour.

fn main() {
    // ── 1. Subprocess tax ──────────────────────────────────────────────────────
    // This MUST be first. CEF re-uses this binary for its subprocesses.
    let cef_path = std::env::var("CEF_PATH")
        .expect("CEF_PATH environment variable must point to the CEF binary distribution");

    if let Some(code) = weld::CefRuntime::execute_process_from(cef_path.as_ref())
        .expect("weld: failed to probe CEF subprocess role — is CEF_PATH correct?")
    {
        std::process::exit(code);
    }

    // ── 2. wgpu instance + window ──────────────────────────────────────────────
    // (stub — winit + wgpu setup not yet wired)
    // In a real integration:
    //   a. Create winit EventLoop + Window.
    //   b. wgpu::Instance::new → request_adapter → request_device.
    //   c. HostWgpuContext::new(device, queue).
    //   d. wgpu surface for the window.

    // ── 3. CEF runtime ─────────────────────────────────────────────────────────
    let config = weld::CefRuntimeConfig::new(&cef_path);
    let runtime = weld::CefRuntime::initialize(config)
        .expect("weld: CEF initialisation failed");

    // ── 4. Browser surface ─────────────────────────────────────────────────────
    // (WindowsCefProducer::new is todo!() until vtable wiring is done)
    //
    // let producer_config = weld::windows_cef::WindowsCefConfig {
    //     surface: weld::CefSurfaceConfig {
    //         initial_url: "https://example.com".into(),
    //         initial_size: PhysicalSize::new(1280, 800),
    //         prefer_accelerated: true,
    //         ..Default::default()
    //     },
    // };
    // let mut producer = weld::windows_cef::WindowsCefProducer::new(&runtime, producer_config)
    //     .expect("weld: failed to create browser surface");

    // ── 5. Event loop ──────────────────────────────────────────────────────────
    // In a real integration, drive CEF's message pump on each winit iteration:
    //   runtime.do_message_loop_work();
    //   if let Ok(Some(texture)) = producer.acquire_frame(&host_ctx) {
    //       // blit `texture` into the wgpu swap-chain frame
    //   }
    //
    // Or, to let CEF own the loop (blocks):
    //   runtime.run_message_loop();

    // For now, spin briefly to confirm the runtime initialised.
    println!("weld demo: CEF runtime initialised (demo stub — no window yet)");
    std::thread::sleep(std::time::Duration::from_millis(200));

    drop(runtime); // triggers cef_shutdown
}
