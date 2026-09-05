// Copyright 2026 Mark Alan Boykin
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0

//! Linux demo: CEF accelerated OSR → wgpu surface via `welding` (DMABUF → Vulkan).
//!
//! # Running
//!
//! ```text
//! export CEF_PATH=/path/to/cef_binary_151.x_linux64
//! export LD_LIBRARY_PATH=$CEF_PATH:$LD_LIBRARY_PATH
//! cargo run -p demo-weld-linux
//! ```
//!
//! Validated against Intel/Mesa + Vulkan + X11, and since 2026-08-16 against
//! **AMD Renoir / RADV** (Mesa 26.1.5) on the wgpu-30 row. NVIDIA proprietary
//! and Wayland are not currently supported by the CEF Linux DMABUF path.
//!
//! AMD needs wgpu 30. CEF supplies `DRM_FORMAT_MOD_INVALID` there rather than
//! an explicit modifier, and importing that needs
//! `VK_EXT_image_drm_format_modifier`, which wgpu only enables from 30 on and
//! surfaces as `VULKAN_EXTERNAL_MEMORY_DMA_BUF`. This demo requests that
//! feature when the adapter has it and creates the unified device through
//! Graft's extension-aware helper; welding then imports the buffer as
//! `DRM_FORMAT_MOD_LINEAR`.
//!
//! # Deterministic pixel validation
//!
//! `WELD_PIXEL_FIXTURE=1` loads an embedded, animated dodger-blue page. With
//! `WELD_EXIT_AFTER_FRAMES`, the demo reads a centered 64x64 block back and
//! requires every pixel to match within a small color tolerance. A mismatch
//! makes the process fail. `WELD_TEXTURE_DUMP=<path.ppm>` still writes the
//! whole imported texture for diagnosis:
//!
//! ```text
//! WELD_TEXTURE_DUMP=/tmp/imported.ppm WELD_EXIT_AFTER_FRAMES=2 cargo run -p demo-weld-linux
//! magick /tmp/imported.ppm /tmp/imported.png
//! ```
//!
//! Ask for at least 2 frames. The first accelerated paint lands before the page
//! does, so probing it reports a partial corner that looks exactly like a
//! tiling bug: on example.com frame 1 gave 12160/16384 bytes non-zero starting
//! with black, and frame 2 gave 16384/16384 starting with `[238,238,238,255]`,
//! which is `#EEEEEE`, the real background.
//!
//! # When the window looks wrong but the texture is right
//!
//! `WELD_PRESENT_DUMP=<path.ppm>` copies the **swapchain** out just before
//! presenting, once, after `WELD_PRESENT_DUMP_AFTER_SECS` (default 8). It
//! needs `COPY_SRC` on the surface, which is requested only when the variable
//! is set.
//!
//! Keep the three instruments distinct, because they answer different
//! questions and confusing them wastes hours:
//!
//! | instrument | answers |
//! | --- | --- |
//! | `WELD_TEXTURE_DUMP` | what CEF handed over |
//! | `WELD_PRESENT_DUMP` | what this application drew |
//! | a screen capture | what the compositor chose to show |
//!
//! On a Wayland session the third is not measurable over SSH at all:
//! `ffmpeg -f x11grab -i :0.0` returns black whatever is on screen, because
//! Wayland surfaces are not in the X root window. Put a known-visible window
//! up as a positive control before believing any such capture.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use winit::{
    application::ApplicationHandler,
    dpi::PhysicalSize,
    event::{ElementState, MouseButton as WinitMouseButton, MouseScrollDelta, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    keyboard::PhysicalKey,
    window::{Window, WindowAttributes},
};

use welding::{
    CefRuntime, CefRuntimeConfig, CefSandboxMode, CefSurfaceConfig, CefSurfaceProducer,
    EventModifiers, FocusDirection, HostWgpuContext, ImportedTexture, KeyEvent, KeyEventKind,
    MouseAction, MouseButton, MouseEvent, PopupSurface,
    linux_cef::{LinuxCefConfig, LinuxCefProducer},
};

mod blit;
mod keys;
mod probe;
mod scripted;

use crate::{blit::build_blit_pipeline, keys::keycode_to_vk};

const PIXEL_FIXTURE_URL: &str = concat!(
    "data:text/html;base64,",
    "PHN0eWxlPmh0bWwsYm9keXttYXJnaW46MDt3aWR0aDoxMDAlO2hlaWdodDoxMDAlO2JhY2tncm91bmQ6IzFlOTBmZn1pe3Bvc2l0aW9uOmZpeGVkO3dpZHRoOjFweDtoZWlnaHQ6MXB4fTwvc3R5bGU+PGk+PC9pPjxzY3JpcHQ+bGV0IG49MDtzZXRJbnRlcnZhbCgoKT0+ZG9jdW1lbnQucXVlcnlTZWxlY3RvcignaScpLnN0eWxlLmJhY2tncm91bmQ9bisrJTI/JyMwMDAnOicjZmZmJywxNik8L3NjcmlwdD4="
);
const PIXEL_TOLERANCE: u8 = 8;

// ── App state ─────────────────────────────────────────────────────────────────

struct DemoApp {
    cef_runtime: Option<CefRuntime>,
    state: Option<DemoState>,
    timeout_at: Option<Instant>,
    fixture_result: Option<bool>,
}

struct DemoState {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    surface_config: wgpu::SurfaceConfiguration,
    host_ctx: HostWgpuContext,
    cef_runtime: CefRuntime,
    producer: LinuxCefProducer,
    pipeline: wgpu::RenderPipeline,
    bg_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    frame: Option<ImportedTexture>,
    frames_imported: u32,
    /// Where a crashed renderer is sent back to.
    recover_url: String,
    battery_started: bool,
    ticks: u32,
    cdp_ticks: u32,
    snapshot_requested: bool,
    snapshot_path: Option<std::path::PathBuf>,
    /// `WELD_PRESENT_DUMP=<path.ppm>`: copy the swapchain out just before
    /// presenting it, once, after `present_dump_at`.
    ///
    /// This answers a question neither of the other instruments can. The
    /// texture dump shows what CEF handed over, and a screen capture shows
    /// what the compositor chose to show, which on Xwayland is a black root
    /// window whatever is really on screen. This shows what the render pass
    /// actually produced, and needs nobody's cooperation.
    present_dump: Option<std::path::PathBuf>,
    present_dump_at: Instant,
    import_errors: u64,
    scripted: scripted::ScriptedInput,
    /// Cached popup widget surface, held across frames because CEF only
    /// repaints it on change and dropped when `popup_rect` goes to `None`.
    popup: Option<PopupSurface>,
    cursor: (f32, f32),
    mods: EventModifiers,
    focus_pending: bool,
    focus_attempted: bool,
}

impl DemoApp {
    fn new(cef_runtime: CefRuntime) -> Self {
        Self {
            cef_runtime: Some(cef_runtime),
            state: None,
            timeout_at: exit_after_seconds().map(|timeout| Instant::now() + timeout),
            fixture_result: None,
        }
    }
}

// ── ApplicationHandler ────────────────────────────────────────────────────────

impl ApplicationHandler for DemoApp {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.state.is_some() {
            return;
        }
        let window = Arc::new(
            el.create_window(
                WindowAttributes::default()
                    .with_title("welding demo — CEF → wgpu (Vulkan DMABUF)")
                    .with_inner_size(PhysicalSize::new(1280u32, 800u32)),
            )
            .expect("window creation failed"),
        );

        let (device, queue, surface, surface_config) = pollster::block_on(async {
            // Force the Vulkan backend: the DMABUF import path requires
            // wgpu's Vulkan HAL. Mesa's Vulkan ICD covers Intel / AMD;
            // NVIDIA's proprietary driver is known not to work with the
            // CEF DMABUF path.
            let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
                backends: wgpu::Backends::VULKAN,
                ..wgpu::InstanceDescriptor::new_without_display_handle()
            });
            let surface = instance.create_surface(window.clone()).unwrap();
            let adapter = instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::HighPerformance,
                    compatible_surface: Some(&surface),
                    force_fallback_adapter: false,
                    // wgpu 30 limit bucketing, off to keep the adapter's real limits.
                    apply_limit_buckets: false,
                })
                .await
                .expect("no suitable wgpu Vulkan adapter");
            // Welding needs both the external-memory stack and
            // VK_EXT_queue_family_foreign. Ask wgpu for the public feature,
            // then let Graft construct the same unified device with its
            // complete native extension set.
            let dmabuf_import = adapter.features() & wgpu::Features::VULKAN_EXTERNAL_MEMORY_DMA_BUF;
            log::info!(
                "adapter DMA-BUF import feature: {}",
                !dmabuf_import.is_empty()
            );
            let (device, queue) = welding::build_dmabuf_capable_device(
                &adapter,
                &wgpu::DeviceDescriptor {
                    label: Some("welding-demo"),
                    required_features: dmabuf_import,
                    required_limits: wgpu::Limits::default(),
                    experimental_features: wgpu::ExperimentalFeatures::disabled(),
                    memory_hints: wgpu::MemoryHints::default(),
                    trace: wgpu::Trace::Off,
                },
            )
            .expect("DMA-BUF-capable device creation failed");
            let caps = surface.get_capabilities(&adapter);
            let fmt = caps
                .formats
                .iter()
                .copied()
                .find(|f| f.is_srgb())
                .unwrap_or(caps.formats[0]);
            let sz = window.inner_size();
            // WELD_PRESENT_DUMP needs to copy the swapchain image out, which
            // needs COPY_SRC on it. Only ask when the dump is wanted, and only
            // when the surface offers it, so the ordinary path configures
            // exactly as it always did.
            let mut usage = wgpu::TextureUsages::RENDER_ATTACHMENT;
            if std::env::var_os("WELD_PRESENT_DUMP").is_some() {
                if caps.usages.contains(wgpu::TextureUsages::COPY_SRC) {
                    usage |= wgpu::TextureUsages::COPY_SRC;
                } else {
                    log::error!(
                        "WELD_PRESENT_DUMP set but this surface does not support COPY_SRC; \
                         the dump will be skipped"
                    );
                }
            }
            let cfg = wgpu::SurfaceConfiguration {
                usage,
                format: fmt,
                width: sz.width.max(1),
                height: sz.height.max(1),
                present_mode: wgpu::PresentMode::AutoVsync,
                // wgpu 30 made surface color space explicit; Auto keeps pre-30 behavior.
                color_space: wgpu::SurfaceColorSpace::Auto,
                alpha_mode: wgpu::CompositeAlphaMode::Auto,
                view_formats: vec![],
                desired_maximum_frame_latency: 2,
            };
            surface.configure(&device, &cfg);
            (device, queue, surface, cfg)
        });

        let host_ctx = HostWgpuContext::new(device, queue);
        log::info!("wgpu interop backend: {:?}", host_ctx.backend);
        let (pipeline, bg_layout, sampler) =
            build_blit_pipeline(&host_ctx.device, surface_config.format);

        let cef_runtime = self.cef_runtime.take().unwrap();
        let win_size = window.inner_size();
        // WELD_SCALE forces a scale factor regardless of the display, which is
        // how the HiDPI path gets exercised on a 1x screen.
        let scale = forced_scale().unwrap_or_else(|| window.scale_factor());
        log::info!(
            "creating CEF browser ({}x{})",
            win_size.width,
            win_size.height
        );
        let url = initial_url();
        let mut producer = LinuxCefProducer::new(
            &cef_runtime,
            LinuxCefConfig {
                surface: CefSurfaceConfig {
                    initial_url: url.clone(),
                    initial_size: win_size,
                    // Physical size plus the display scale: CEF lays out at
                    // size/scale CSS pixels and paints the full physical size.
                    scale_factor: scale as f32,
                    background_color: env_background(),
                    // WELD_DOWNLOAD_DIR=path accepts downloads into that
                    // directory; unset refuses them, which is the default.
                    download_dir: std::env::var("WELD_DOWNLOAD_DIR").ok().map(Into::into),
                    // WELD_PROFILE must be an absolute subdirectory of
                    // WELD_CACHE_ROOT, which CEF uses as the root cache.
                    user_data_dir: std::env::var_os("WELD_PROFILE").map(Into::into),
                    devtools_protocol: std::env::var("WELD_CDP").is_ok(),
                    // WELD_AUTH=user:pass answers auth challenges; unset
                    // declines them, which is the default.
                    handle_auth_challenges: std::env::var("WELD_AUTH").is_ok(),
                    // WELD_PERMISSIONS=grant|deny answers permission requests;
                    // unset denies them, which is the default.
                    handle_permission_requests: std::env::var("WELD_PERMISSIONS").is_ok(),
                    ..Default::default()
                },
            },
        )
        .expect("failed to create CEF browser surface");
        log::info!("CEF browser created");
        // CEF starts a windowless browser in an unspecified visibility state;
        // say it plainly.
        if let Err(err) = producer.set_visible(true) {
            log::warn!("initial set_visible failed: {err}");
        }

        self.state = Some(DemoState {
            window,
            surface,
            surface_config,
            host_ctx,
            cef_runtime,
            producer,
            recover_url: url,
            pipeline,
            bg_layout,
            sampler,
            frame: None,
            frames_imported: 0,
            battery_started: false,
            ticks: 0,
            cdp_ticks: 0,
            snapshot_requested: false,
            snapshot_path: std::env::var_os("WELD_SNAPSHOT").map(Into::into),
            present_dump: std::env::var_os("WELD_PRESENT_DUMP").map(Into::into),
            // Default late enough to be well past the last paint of a static
            // page, since the steady state is the interesting one.
            present_dump_at: Instant::now()
                + Duration::from_secs(
                    std::env::var("WELD_PRESENT_DUMP_AFTER_SECS")
                        .ok()
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(8),
                ),
            import_errors: 0,
            scripted: scripted::ScriptedInput::from_env(),
            popup: None,
            cursor: (0.0, 0.0),
            mods: EventModifiers::default(),
            focus_pending: true,
            focus_attempted: false,
        });
        // Xwayland does not guarantee an initial redraw for a window created
        // from a non-interactive SSH session. Start the poll/render loop
        // explicitly; subsequent redraws schedule themselves.
        self.state.as_ref().unwrap().window.request_redraw();
    }

    fn window_event(
        &mut self,
        el: &ActiveEventLoop,
        _win_id: winit::window::WindowId,
        event: WindowEvent,
    ) {
        let s = match self.state.as_mut() {
            Some(s) => s,
            None => return,
        };
        match event {
            WindowEvent::CloseRequested => {
                let _ = s.producer.close();
                el.exit();
            }

            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                // The window crossed onto a display with a different density.
                // A forced WELD_SCALE outranks it: on a 1x panel winit reports
                // 1.0 here and would undo the very override being tested.
                if forced_scale().is_some() {
                    log::info!(
                        "ScaleFactorChanged({scale_factor}) ignored: WELD_SCALE pins the scale"
                    );
                } else if let Err(err) = s.producer.set_scale_factor(scale_factor as f32) {
                    log_scale_err(err);
                }
            }

            WindowEvent::Resized(size) => {
                s.surface_config.width = size.width.max(1);
                s.surface_config.height = size.height.max(1);
                s.surface.configure(&s.host_ctx.device, &s.surface_config);
                let _ = s.producer.resize(size);
            }

            WindowEvent::Focused(true) => {
                s.focus_pending = true;
                s.focus_attempted = false;
            }

            WindowEvent::ModifiersChanged(m) => {
                s.mods.shift = m.state().shift_key();
                s.mods.ctrl = m.state().control_key();
                s.mods.alt = m.state().alt_key();
                s.mods.meta = m.state().super_key();
            }

            WindowEvent::KeyboardInput { event: ke, .. } => {
                if s.frames_imported == 0 {
                    return;
                }
                let PhysicalKey::Code(kc) = ke.physical_key else {
                    return;
                };
                let vk = keycode_to_vk(kc);
                if ke.state == ElementState::Pressed {
                    let _ = s.producer.send_keyboard_input(KeyEvent {
                        kind: KeyEventKind::RawKeyDown,
                        windows_key_code: vk,
                        native_key_code: 0,
                        character: None,
                        modifiers: s.mods,
                    });
                    if let Some(ch) = ke.text.as_ref().and_then(|t| t.chars().next()) {
                        let _ = s.producer.send_keyboard_input(KeyEvent {
                            kind: KeyEventKind::Char,
                            windows_key_code: vk,
                            native_key_code: 0,
                            character: Some(ch),
                            modifiers: s.mods,
                        });
                    }
                } else {
                    let _ = s.producer.send_keyboard_input(KeyEvent {
                        kind: KeyEventKind::KeyUp,
                        windows_key_code: vk,
                        native_key_code: 0,
                        character: None,
                        modifiers: s.mods,
                    });
                }
            }

            WindowEvent::CursorMoved { position, .. } => {
                // Separates "winit never delivered it" from "CEF ignored it".
                log::debug!(
                    "winit CursorMoved {},{}",
                    position.x as i32,
                    position.y as i32
                );
                s.cursor = (position.x as f32, position.y as f32);
                if s.frames_imported == 0 {
                    return;
                }
                let _ = s.producer.send_mouse_input(MouseEvent {
                    x: position.x as i32,
                    y: position.y as i32,
                    button: MouseButton::Left,
                    action: MouseAction::Moved,
                    modifiers: s.mods,
                });
            }

            WindowEvent::MouseInput { state, button, .. } => {
                log::info!("winit MouseInput {state:?} {button:?} at {:?}", s.cursor);
                let mb = match button {
                    WinitMouseButton::Left => MouseButton::Left,
                    WinitMouseButton::Right => MouseButton::Right,
                    WinitMouseButton::Middle => MouseButton::Middle,
                    _ => return,
                };
                let action = if state == ElementState::Pressed {
                    MouseAction::Pressed
                } else {
                    MouseAction::Released
                };
                match mb {
                    MouseButton::Left => s.mods.left_mouse_button = state == ElementState::Pressed,
                    MouseButton::Middle => {
                        s.mods.middle_mouse_button = state == ElementState::Pressed
                    }
                    MouseButton::Right => {
                        s.mods.right_mouse_button = state == ElementState::Pressed
                    }
                }
                if s.frames_imported == 0 {
                    return;
                }
                match s.producer.send_mouse_input(MouseEvent {
                    x: s.cursor.0 as i32,
                    y: s.cursor.1 as i32,
                    button: mb,
                    action,
                    modifiers: s.mods,
                }) {
                    Ok(()) => log::info!("send_mouse_input(click) -> Ok"),
                    Err(err) => log::error!("send_mouse_input(click) -> {err}"),
                }
            }

            WindowEvent::MouseWheel { delta, .. } => {
                if s.frames_imported == 0 {
                    return;
                }
                let (dx, dy) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => ((x * 20.0) as i32, (y * 20.0) as i32),
                    MouseScrollDelta::PixelDelta(d) => (d.x as i32, d.y as i32),
                };
                let _ = s.producer.send_mouse_input(MouseEvent {
                    x: s.cursor.0 as i32,
                    y: s.cursor.1 as i32,
                    button: MouseButton::Left,
                    action: MouseAction::WheelScrolled {
                        delta_x: dx,
                        delta_y: dy,
                    },
                    modifiers: s.mods,
                });
            }

            WindowEvent::RedrawRequested => {
                s.cef_runtime.do_message_loop_work();

                if self
                    .timeout_at
                    .is_some_and(|timeout_at| Instant::now() >= timeout_at)
                {
                    log::info!("gracefully closing after configured timeout");
                    // Report here too, as demo-weld-mac does. Without it a run
                    // that times out waiting for frames which never arrive
                    // says nothing about the frames it did import, and a
                    // report taken long after the last paint is exactly what
                    // tells a decayed buffer from a fresh one.
                    self.fixture_result = Some(report(s));
                    let _ = s.producer.close();
                    el.exit();
                    return;
                }

                match s.producer.acquire_frame(&s.host_ctx) {
                    Ok(Some(new_frame)) => {
                        s.frames_imported += 1;
                        log::info!(
                            "imported frame #{} ({}x{} {:?})",
                            s.frames_imported,
                            new_frame.size.width,
                            new_frame.size.height,
                            new_frame.format
                        );
                        s.frame = Some(new_frame);
                    }
                    Ok(None) => {}
                    Err(e) => {
                        // Rate-limited on purpose. A GPU that cannot import
                        // CEF's buffer fails on *every* paint, and an animating
                        // page paints forever: logging each one filled a 7.5G
                        // tmpfs during an IME test and took the run with it.
                        s.import_errors += 1;
                        if s.import_errors == 1 || s.import_errors % 500 == 0 {
                            log::error!("acquire_frame error (x{}): {e}", s.import_errors);
                        }
                    }
                }

                if s.focus_pending && !s.focus_attempted && s.frames_imported > 0 {
                    s.focus_attempted = true;
                    match s.producer.move_focus(FocusDirection::Forward) {
                        Ok(()) => s.focus_pending = false,
                        Err(err) => log::warn!("deferred move_focus failed: {err}"),
                    }
                }

                // Popup widget surface. CEF paints it separately from the view
                // and hides it without painting, so both questions get asked
                // every frame.
                match s.producer.acquire_popup(&s.host_ctx) {
                    Ok(Some(popup)) => {
                        log::info!(
                            "imported popup {}x{} at {},{}",
                            popup.rect.width,
                            popup.rect.height,
                            popup.rect.x,
                            popup.rect.y
                        );
                        s.popup = Some(popup);
                    }
                    Ok(None) => {}
                    Err(e) => log::error!("acquire_popup error: {e}"),
                }
                if s.producer.popup_rect().is_none() && s.popup.take().is_some() {
                    log::info!("popup closed");
                }

                // The page owns the cursor's meaning; the host owns the
                // pointer. Nothing shows an I-beam unless we apply it.
                if let Some(shape) = s.producer.poll_cursor_shape() {
                    log::info!("cursor -> {shape:?}");
                    s.window.set_cursor(winit_cursor(&shape));
                }

                while let Some(event) = s.producer.poll_web_event() {
                    match event {
                        welding::CefSurfaceEvent::Navigation(event) => {
                            log::info!("nav: {event:?}");
                            scripted::recover_if_crashed(&mut s.producer, &s.recover_url, &event);
                            scripted::answer_auth_if_challenged(&mut s.producer, &event);
                            scripted::answer_permission_if_asked(&mut s.producer, &event);
                            scripted::finish_page_drag_if_started(&mut s.producer, &event);
                        }
                        welding::CefSurfaceEvent::WebMessage(message) => {
                            log::info!("WEB MESSAGE => {message}");
                        }
                        welding::CefSurfaceEvent::ScriptCompleted { id, result } => match result {
                            Ok(json) => log::info!("SCRIPT #{} => {json}", id.get()),
                            Err(err) => log::error!("SCRIPT #{} threw: {err}", id.get()),
                        },
                        welding::CefSurfaceEvent::CookiesCompleted { id, result } => match result {
                            Ok(cookies) => {
                                log::info!("COOKIES #{} n={}", id.get(), cookies.len());
                                for c in cookies.iter().take(3) {
                                    log::info!("  {}={} domain={}", c.name, c.value, c.domain);
                                }
                            }
                            Err(err) => {
                                log::error!("COOKIES #{} failed: {err}", id.get());
                            }
                        },
                        _ => {}
                    }
                }

                // Parity battery: one run reports frames, script results,
                // HiDPI layout and cookies, so the same evidence exists on
                // every platform.
                s.ticks += 1;
                if s.cdp_ticks == 200 || s.ticks == 200 {
                    if let Ok(text) = std::env::var("WELD_FIND") {
                        match s.producer.find(&text, true, false, false) {
                            Ok(()) => eprintln!("weld demo: find {text:?}"),
                            Err(e) => eprintln!("weld demo: find failed: {e}"),
                        }
                    }
                    if let Ok(pdf) = std::env::var("WELD_PDF") {
                        match s.producer.print_to_pdf(std::path::Path::new(&pdf)) {
                            Ok(()) => eprintln!("weld demo: print_to_pdf {pdf}"),
                            Err(e) => eprintln!("weld demo: print_to_pdf failed: {e}"),
                        }
                    }
                    if std::env::var("WELD_PRINT").is_ok() {
                        match s.producer.print() {
                            Ok(()) => eprintln!("weld demo: print dialog requested"),
                            Err(e) => eprintln!("weld demo: print failed: {e}"),
                        }
                    }
                    if std::env::var("WELD_ZOOM").is_ok() {
                        let _ = s.producer.zoom(welding::ZoomCommand::In);
                        let _ = s.producer.zoom(welding::ZoomCommand::In);
                    }
                    eprintln!(
                        "weld demo: can_go_back={} can_go_forward={}",
                        s.producer.can_go_back(),
                        s.producer.can_go_forward()
                    );
                }
                if !s.snapshot_requested && s.ticks > 90 && s.snapshot_path.is_some() {
                    s.snapshot_requested = true;
                    match s.producer.request_snapshot_png() {
                        Ok(id) => eprintln!("weld demo: PNG snapshot #{id} requested"),
                        Err(e) => eprintln!("weld demo: snapshot request failed: {e}"),
                    }
                }
                // WELD_CDP=<method> sends one CDP call, then prints every
                // reply and event as it arrives.
                if let Ok(method) = std::env::var("WELD_CDP") {
                    s.cdp_ticks += 1;
                    if s.cdp_ticks == 200 {
                        let json = format!(r#"{{"id":1,"method":"{method}"}}"#);
                        match s.producer.send_devtools_message(&json) {
                            Ok(()) => eprintln!("weld demo: CDP sent {json}"),
                            Err(e) => eprintln!("weld demo: CDP send failed: {e}"),
                        }
                    }
                    while let Some(msg) = s.producer.poll_devtools_message() {
                        eprintln!("weld demo: CDP <- {}", &msg[..msg.len().min(110)]);
                    }
                }
                // The scripted gestures, for a machine nobody is sitting at.
                s.scripted.tick(&mut s.producer, s.frames_imported > 0);
                // Ticks, not imported frames: accelerated OSR only paints on
                // change, so a static page yields one frame and the battery
                // would never fire.
                if !s.battery_started && s.ticks > 60 {
                    s.battery_started = true;
                    if let Ok(script) = std::env::var("WELD_SCRIPT") {
                        let id = welding::WebRequestId::new(4_294_967_296);
                        match s.producer.request_script_result(id, &script) {
                            Ok(()) => log::info!("script request #{}", id.get()),
                            Err(e) => log::error!("request_script_result failed: {e}"),
                        }
                    }
                    if let Ok(url) = std::env::var("WELD_COOKIE_URL") {
                        let probe = welding::Cookie {
                            name: "weld_probe".into(),
                            value: "parity".into(),
                            domain: "example.com".into(),
                            path: "/".into(),
                            ..Default::default()
                        };
                        match s.producer.set_cookie(&url, &probe) {
                            Ok(()) => log::info!("set_cookie accepted"),
                            Err(e) => log::error!("set_cookie failed: {e}"),
                        }
                        let id = welding::WebRequestId::new(4_294_967_297);
                        if let Err(e) = s.producer.request_cookies(id, Some(&url)) {
                            log::error!("request_cookies failed: {e}");
                        }
                    }
                }
                if let Some(completion) = s.producer.poll_snapshot_png() {
                    let snapshot_id = completion.id;
                    match completion.result {
                        Ok(bytes) => {
                            if let Some(path) = s.snapshot_path.as_ref() {
                                match std::fs::write(path, &bytes) {
                                    Ok(()) if bytes.starts_with(b"\x89PNG\r\n\x1a\n") => {
                                        eprintln!(
                                            "weld demo: PNG snapshot #{snapshot_id} {} bytes -> {}",
                                            bytes.len(),
                                            path.display()
                                        );
                                    }
                                    Ok(()) => eprintln!("weld demo: snapshot was not a PNG"),
                                    Err(e) => eprintln!("weld demo: could not write snapshot: {e}"),
                                }
                            }
                        }
                        Err(e) => eprintln!("weld demo: snapshot #{snapshot_id} failed: {e}"),
                    }
                }
                // Unattended verdict, the same instrument demo-weld-mac uses:
                // a window nobody is watching proves nothing, so read the
                // imported pixels back and say what they were.
                if let Some(limit) = exit_after_frames() {
                    if s.frames_imported >= limit {
                        self.fixture_result = Some(report(s));
                        let _ = s.producer.close();
                        el.exit();
                        return;
                    }
                }

                let output = match s.surface.get_current_texture() {
                    wgpu::CurrentSurfaceTexture::Success(t)
                    | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
                    wgpu::CurrentSurfaceTexture::Lost | wgpu::CurrentSurfaceTexture::Outdated => {
                        s.surface.configure(&s.host_ctx.device, &s.surface_config);
                        s.window.request_redraw();
                        return;
                    }
                    wgpu::CurrentSurfaceTexture::Timeout
                    | wgpu::CurrentSurfaceTexture::Occluded => {
                        s.window.request_redraw();
                        return;
                    }
                    wgpu::CurrentSurfaceTexture::Validation => {
                        log::error!("surface validation error");
                        return;
                    }
                };

                let target = output
                    .texture
                    .create_view(&wgpu::TextureViewDescriptor::default());
                let mut enc =
                    s.host_ctx
                        .device
                        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("blit"),
                        });

                let make_bg = |view: &wgpu::TextureView| {
                    s.host_ctx
                        .device
                        .create_bind_group(&wgpu::BindGroupDescriptor {
                            label: None,
                            layout: &s.bg_layout,
                            entries: &[
                                wgpu::BindGroupEntry {
                                    binding: 0,
                                    resource: wgpu::BindingResource::TextureView(view),
                                },
                                wgpu::BindGroupEntry {
                                    binding: 1,
                                    resource: wgpu::BindingResource::Sampler(&s.sampler),
                                },
                            ],
                        })
                };
                let bg = s.frame.as_ref().map(|f| make_bg(&f.view));
                let popup_bg = s.popup.as_ref().map(|p| make_bg(&p.texture.view));

                {
                    let clear_color = if bg.is_some() {
                        wgpu::Color::BLACK
                    } else {
                        wgpu::Color {
                            r: 0.1,
                            g: 0.1,
                            b: 0.1,
                            a: 1.0,
                        }
                    };
                    let mut rpass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("blit"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &target,
                            resolve_target: None,
                            depth_slice: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(clear_color),
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        ..Default::default()
                    });
                    if let Some(bg) = &bg {
                        rpass.set_pipeline(&s.pipeline);
                        rpass.set_bind_group(0, bg, &[]);
                        rpass.draw(0..3, 0..1);
                    }

                    // Popup widget (select dropdown, autocomplete) over the
                    // view, clipped to the rect CEF asked for. Same pipeline,
                    // different viewport.
                    if let (Some(popup), Some(popup_bg)) = (&s.popup, &popup_bg) {
                        let vw = s.surface_config.width as f32;
                        let vh = s.surface_config.height as f32;
                        let x = (popup.rect.x as f32).clamp(0.0, vw);
                        let y = (popup.rect.y as f32).clamp(0.0, vh);
                        let w = (popup.rect.width as f32).min(vw - x);
                        let h = (popup.rect.height as f32).min(vh - y);
                        if w > 0.0 && h > 0.0 {
                            rpass.set_viewport(x, y, w, h, 0.0, 1.0);
                            rpass.set_pipeline(&s.pipeline);
                            rpass.set_bind_group(0, popup_bg, &[]);
                            rpass.draw(0..3, 0..1);
                        }
                    }
                }

                s.host_ctx.queue.submit([enc.finish()]);

                // Read the swapchain back before it goes to the compositor.
                // Whatever is in here is what the render pass produced, so a
                // black image means the blit failed and a page means it did
                // not, with no compositor or screen-capture in the path.
                if s.present_dump.is_some() && Instant::now() >= s.present_dump_at {
                    let path = s.present_dump.take().expect("checked is_some");
                    match probe::dump_ppm(
                        &s.host_ctx.device,
                        &s.host_ctx.queue,
                        &output.texture,
                        &path.to_string_lossy(),
                    ) {
                        Ok(()) => log::info!(
                            "present dump -> {} ({} frame(s) imported, holding a frame: {})",
                            path.display(),
                            s.frames_imported,
                            s.frame.is_some()
                        ),
                        Err(e) => log::error!("present dump failed: {e}"),
                    }
                }

                // wgpu 30 moved presentation from SurfaceTexture to Queue.
                s.host_ctx.queue.present(output);
                s.window.request_redraw();
            }

            _ => {}
        }
    }
}

// ── main ──────────────────────────────────────────────────────────────────────

fn main() {
    // MUST be first: CEF re-invokes this binary for renderer/GPU/utility subprocesses.
    let cef_path = std::env::var("CEF_PATH")
        .expect("CEF_PATH must point to the CEF binary distribution (contains libcef.so)");
    let sandbox = match std::env::var("WELD_SANDBOX").as_deref() {
        Ok("sandboxed") => CefSandboxMode::Sandboxed,
        Ok(value) => panic!("WELD_SANDBOX must be 'sandboxed' or unset, got {value:?}"),
        Err(std::env::VarError::NotPresent) => CefSandboxMode::UnsandboxedTrustedContent,
        Err(error) => panic!("WELD_SANDBOX is not valid Unicode: {error}"),
    };
    if let Some(code) = CefRuntime::execute_process_from(cef_path.as_ref(), sandbox)
        .expect("welding: CEF subprocess probe failed — is CEF_PATH set correctly?")
    {
        std::process::exit(code);
    }

    // env_logger is initialised after the subprocess fork-guard so the noisy
    // CEF renderer/GPU helper processes don't all init their own logger.
    env_logger::init();

    let mut config = CefRuntimeConfig::new(&cef_path, sandbox);
    // WELD_CACHE_ROOT is the CEF root cache. WELD_PROFILE must name a child
    // path, but its final directory must be left for CEF to create.
    config.cache_path = std::env::var_os("WELD_CACHE_ROOT")
        .map(Into::into)
        .or_else(|| Some(std::env::temp_dir().join("welding-demo-linux-cache")));
    config.user_agent = std::env::var("WELD_UA").ok();
    config.user_agent_product = std::env::var("WELD_UA_PRODUCT").ok();
    // A fresh CEF profile otherwise blocks inside CefInitialize on Chromium's
    // first-run EULA, before winit can create a window or enforce the demo
    // timeout. Hosts may add further switches through WELD_SWITCHES.
    config.command_line_switches = vec![
        ("no-first-run".into(), None),
        ("no-default-browser-check".into(), None),
    ];
    // WELD_SWITCHES=disable-popup-blocking,lang=en-GB
    if let Ok(list) = std::env::var("WELD_SWITCHES") {
        config
            .command_line_switches
            .extend(
                list.split(',')
                    .filter(|s| !s.is_empty())
                    .map(|s| match s.split_once('=') {
                        Some((k, v)) => (k.to_owned(), Some(v.to_owned())),
                        None => (s.to_owned(), None),
                    }),
            );
    }
    eprintln!("weld demo: switches {:?}", config.command_line_switches);
    let runtime = CefRuntime::initialize(config).expect("welding: CEF initialize failed");

    let event_loop = EventLoop::new().expect("event loop creation failed");
    event_loop.set_control_flow(ControlFlow::Poll);

    let mut app = DemoApp::new(runtime);
    event_loop.run_app(&mut app).expect("event loop error");
    if pixel_fixture_enabled() && app.fixture_result != Some(true) {
        std::process::exit(1);
    }
}

/// Shared vocabulary to winit's icons.
fn winit_cursor(shape: &welding::CursorShape) -> winit::window::Cursor {
    use welding::CursorShape as S;
    use winit::window::CursorIcon as I;
    let icon = match shape {
        S::Default => I::Default,
        S::Pointer => I::Pointer,
        S::Text => I::Text,
        S::Wait => I::Wait,
        S::Crosshair => I::Crosshair,
        S::Move => I::Move,
        S::NotAllowed => I::NotAllowed,
        S::Help => I::Help,
        S::Progress => I::Progress,
        S::ResizeNs => I::NsResize,
        S::ResizeEw => I::EwResize,
        S::ResizeNeSw => I::NeswResize,
        S::ResizeNwSe => I::NwseResize,
        S::ResizeAll => I::AllScroll,
        S::Grab => I::Grab,
        S::Grabbing => I::Grabbing,
        S::ZoomIn => I::ZoomIn,
        S::ZoomOut => I::ZoomOut,
        S::Custom(_) => I::Default,
        _ => I::Default,
    };
    icon.into()
}

/// `WELD_EXIT_AFTER_FRAMES=N`: probe frame N and exit.
fn exit_after_frames() -> Option<u32> {
    std::env::var("WELD_EXIT_AFTER_FRAMES")
        .ok()
        .and_then(|v| v.parse().ok())
}

/// `WELD_TIMEOUT_SECS=N`: end a scripted run even if the GPU cannot import
/// CEF's frame. This preserves input and browser receipts on AMD/RADV hosts
/// whose DMABUF modifier is currently unsupported by wgpu.
fn exit_after_seconds() -> Option<Duration> {
    std::env::var("WELD_TIMEOUT_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_secs)
}

/// Read the center of the imported texture back and report what landed there.
fn report(s: &mut DemoState) -> bool {
    match s.frame.as_ref() {
        Some(frame) => {
            if let Ok(path) = std::env::var("WELD_TEXTURE_DUMP") {
                match probe::dump_ppm(&s.host_ctx.device, &s.host_ctx.queue, &frame.texture, &path)
                {
                    Ok(()) => log::info!("texture dump -> {path}"),
                    Err(e) => log::error!("texture dump failed: {e}"),
                }
            }
            match probe::sample(&s.host_ctx.device, &s.host_ctx.queue, &frame.texture) {
                Ok(rb) if pixel_fixture_enabled() => {
                    let Some(expected) = pixel_fixture_expected(frame.format) else {
                        log::error!(
                            "PIXEL FIXTURE FAIL: unsupported texture format {:?}",
                            frame.format
                        );
                        return false;
                    };
                    let matched = rb.matching_pixels(expected, PIXEL_TOLERANCE);
                    let total = rb.total_pixels();
                    if matched == total {
                        log::info!(
                            "PIXEL FIXTURE PASS: {matched}/{total} center pixels at {:?} matched {:?} ±{PIXEL_TOLERANCE}",
                            rb.origin,
                            expected
                        );
                        true
                    } else {
                        log::error!(
                            "PIXEL FIXTURE FAIL: {matched}/{total} center pixels at {:?} matched {:?} ±{PIXEL_TOLERANCE}; first pixels {:?}",
                            rb.origin,
                            expected,
                            rb.first_pixels
                        );
                        false
                    }
                }
                Ok(rb) if rb.looks_painted() => {
                    log::info!(
                        "VALIDATION PASS: {} frame(s) imported, {}/{} bytes non-zero, center {:?}, first pixels {:?}",
                        s.frames_imported,
                        rb.non_zero_bytes,
                        rb.total_bytes,
                        rb.origin,
                        rb.first_pixels
                    );
                    true
                }
                Ok(rb) => {
                    log::error!(
                        "VALIDATION FAIL: imported but entirely zero ({} bytes)",
                        rb.total_bytes
                    );
                    false
                }
                Err(e) => {
                    log::error!("VALIDATION FAIL: readback failed: {e}");
                    false
                }
            }
        }
        None => {
            log::error!("VALIDATION FAIL: no frame was ever imported");
            false
        }
    }
}

fn pixel_fixture_enabled() -> bool {
    std::env::var_os("WELD_PIXEL_FIXTURE").is_some()
}

fn initial_url() -> String {
    if pixel_fixture_enabled() {
        PIXEL_FIXTURE_URL.into()
    } else {
        std::env::var("WELD_URL").unwrap_or_else(|_| "https://example.com".into())
    }
}

fn pixel_fixture_expected(format: wgpu::TextureFormat) -> Option<[u8; 4]> {
    match format {
        wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb => {
            Some([255, 144, 30, 255])
        }
        wgpu::TextureFormat::Rgba8Unorm | wgpu::TextureFormat::Rgba8UnormSrgb => {
            Some([30, 144, 255, 255])
        }
        _ => None,
    }
}

/// WELD_SCALE pins the scale factor for testing HiDPI on a 1x display. When it
/// is set it has to survive winit's ScaleFactorChanged, which reports the real
/// display density.
fn forced_scale() -> Option<f64> {
    std::env::var("WELD_SCALE")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
}

/// `WELD_BACKGROUND=transparent` for alpha-0 windowless painting,
/// `WELD_BACKGROUND=rrggbb` for an opaque colour; unset or unparsable is the
/// library default (opaque white).
fn env_background() -> Option<[u8; 3]> {
    match std::env::var("WELD_BACKGROUND") {
        Ok(v) if v.eq_ignore_ascii_case("transparent") => None,
        Ok(v) => u32::from_str_radix(v.trim_start_matches('#'), 16)
            .ok()
            .map(|n| [(n >> 16) as u8, (n >> 8) as u8, n as u8])
            .or(Some([255, 255, 255])),
        Err(_) => Some([255, 255, 255]),
    }
}

fn log_scale_err(err: welding::WeldError) {
    log::error!("set_scale_factor failed: {err}");
}
