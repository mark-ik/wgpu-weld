//! macOS demo: CEF accelerated OSR → wgpu surface via `welding`
//! (IOSurface → MTLTexture → wgpu Metal).
//!
//! # Running
//!
//! CEF on macOS will not run from a bare executable. It needs a real `.app`
//! with the framework in `Contents/Frameworks` and the five Helper bundles
//! beside it, so build the bundle rather than `cargo run`:
//!
//! ```text
//! cd demo-weld-mac && cargo run --bin bundle-demo-weld-mac
//! open ../target/bundle/demo-weld-mac.app
//! ```
//!
//! # Unattended validation
//!
//! `WELD_EXIT_AFTER_FRAMES=N` runs N imported frames, probes the last one for
//! real pixels, logs a verdict and exits. That is what makes this useful over
//! SSH, where nobody can see the window. `WELD_URL` overrides the page, and
//! `WELD_TIMEOUT_SECS` (default 60) reports and exits even if the frames never
//! arrive.
//!
//! Note that N should be small. Accelerated OSR only paints on change, so a
//! static page delivers one frame and then goes quiet; asking for 30 frames of
//! example.com waits forever.

mod blit;
mod keys;
mod present;
mod probe;
mod scripted;

use std::{
    io::Write,
    sync::Arc,
    time::{Duration, Instant},
};

use winit::{
    application::ApplicationHandler,
    dpi::PhysicalSize,
    event::{ElementState, MouseButton as WinitMouseButton, MouseScrollDelta, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    keyboard::PhysicalKey,
    platform::pump_events::{EventLoopExtPumpEvents, PumpStatus},
    window::{Window, WindowAttributes},
};

use welding::{
    CefRuntime, CefRuntimeConfig, CefSurfaceConfig, CefSurfaceProducer, EventModifiers,
    FocusDirection, HostWgpuContext, ImportedTexture, KeyEvent, KeyEventKind, MouseAction,
    MouseButton, MouseEvent, PopupSurface,
    macos_cef::{MacosCefConfig, MacosCefProducer, PreparedMacosCefProfile},
};

use crate::keys::keycode_to_vk;

struct DemoApp {
    cef_runtime: Option<CefRuntime>,
    pending: Option<PendingState>,
    state: Option<DemoState>,
    exit_after_frames: Option<u32>,
    exit_after_popups: Option<u32>,
    should_exit: bool,
    /// `WELD_CLICK_AT=x,y`: click once, a few ticks after the first frame.
    ///
    /// Without this there is no way to prove anything that needs a real user
    /// gesture on a machine nobody is sitting at: a `<select>` dropdown will
    /// not open, and Chromium's popup blocker swallows `window.open` before
    /// `on_before_popup` is ever reached.
    scripted: scripted::ScriptedInput,
}

/// The native window and Metal surface are created in winit's `resumed`
/// callback, but a persistent CEF request context must be pumped once before
/// its browser is created. Holding the native half here lets `tick` finish
/// that work between winit dispatches.
struct PendingState {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    surface_config: wgpu::SurfaceConfiguration,
    host_ctx: HostWgpuContext,
    cef_runtime: CefRuntime,
    config: MacosCefConfig,
    profile: PreparedMacosCefProfile,
    blit: blit::Blit,
    recover_url: String,
    snapshot_path: Option<std::path::PathBuf>,
}

struct DemoState {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    surface_config: wgpu::SurfaceConfiguration,
    host_ctx: HostWgpuContext,
    cef_runtime: CefRuntime,
    producer: MacosCefProducer,
    blit: blit::Blit,
    frame: Option<ImportedTexture>,
    /// Cached popup widget surface. Held across frames because CEF only
    /// repaints it on change, and dropped when `popup_rect` goes to `None`.
    popup: Option<PopupSurface>,
    frames_imported: u32,
    /// Where a crashed renderer is sent back to.
    recover_url: String,
    battery_started: bool,
    ticks: u32,
    cdp_ticks: u32,
    snapshot_requested: bool,
    snapshot_path: Option<std::path::PathBuf>,
    popups_imported: u32,
    cursor: (f32, f32),
    mods: EventModifiers,
    started_at: Instant,
    history_checked: bool,
}

impl ApplicationHandler for DemoApp {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.state.is_some() || self.pending.is_some() {
            return;
        }
        let window = Arc::new(
            el.create_window(
                WindowAttributes::default()
                    .with_title("welding demo — CEF → wgpu (Metal IOSurface)")
                    .with_inner_size(PhysicalSize::new(1280u32, 800u32)),
            )
            .expect("window creation failed"),
        );

        let (device, queue, surface, surface_config) = pollster::block_on(async {
            // Metal only: the IOSurface import path goes through wgpu's Metal HAL.
            let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
                backends: wgpu::Backends::METAL,
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
                .expect("no suitable wgpu Metal adapter");
            let (device, queue) = adapter
                .request_device(&wgpu::DeviceDescriptor {
                    label: Some("welding-demo"),
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::default(),
                    experimental_features: wgpu::ExperimentalFeatures::disabled(),
                    memory_hints: wgpu::MemoryHints::default(),
                    trace: wgpu::Trace::Off,
                })
                .await
                .expect("request_device failed");
            let caps = surface.get_capabilities(&adapter);
            let fmt = caps
                .formats
                .iter()
                .copied()
                .find(|f| f.is_srgb())
                .unwrap_or(caps.formats[0]);
            let sz = window.inner_size();
            let cfg = wgpu::SurfaceConfiguration {
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
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
        let blit = blit::build(&host_ctx.device, surface_config.format);

        let cef_runtime = self.cef_runtime.take().unwrap();
        let win_size = window.inner_size();
        // WELD_SCALE forces a scale factor regardless of the display, which is
        // how the HiDPI path gets exercised on a 1x screen.
        let scale = forced_scale().unwrap_or_else(|| window.scale_factor());
        let url = std::env::var("WELD_URL").unwrap_or_else(|_| "https://example.com".into());
        let config = MacosCefConfig {
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
        };
        let profile = match MacosCefProducer::prepare_profile(&cef_runtime, &config) {
            Ok(profile) => profile,
            Err(err) => {
                log::error!("failed to prepare CEF profile: {err}");
                self.cef_runtime = Some(cef_runtime);
                self.should_exit = true;
                return;
            }
        };
        self.pending = Some(PendingState {
            window,
            surface,
            surface_config,
            host_ctx,
            cef_runtime,
            config,
            profile,
            blit,
            recover_url: url,
            snapshot_path: std::env::var_os("WELD_SNAPSHOT").map(Into::into),
        });
    }

    fn window_event(
        &mut self,
        _el: &ActiveEventLoop,
        _win_id: winit::window::WindowId,
        event: WindowEvent,
    ) {
        let mut close_requested = false;
        let s = match self.state.as_mut() {
            Some(s) => s,
            None => return,
        };
        match event {
            WindowEvent::CloseRequested => {
                let _ = s.producer.close();
                close_requested = true;
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
                let _ = s.producer.move_focus(FocusDirection::Forward);
            }

            WindowEvent::ModifiersChanged(m) => {
                s.mods.shift = m.state().shift_key();
                s.mods.ctrl = m.state().control_key();
                s.mods.alt = m.state().alt_key();
                s.mods.meta = m.state().super_key();
            }

            WindowEvent::KeyboardInput { event: ke, .. } => {
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
                s.cursor = (position.x as f32, position.y as f32);
                let _ = s.producer.send_mouse_input(MouseEvent {
                    x: position.x as i32,
                    y: position.y as i32,
                    button: MouseButton::Left,
                    action: MouseAction::Moved,
                    modifiers: s.mods,
                });
            }

            WindowEvent::MouseInput { state, button, .. } => {
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
                let _ = s.producer.send_mouse_input(MouseEvent {
                    x: s.cursor.0 as i32,
                    y: s.cursor.1 as i32,
                    button: mb,
                    action,
                    modifiers: s.mods,
                });
            }

            WindowEvent::MouseWheel { delta, .. } => {
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

            // Deliberately no RedrawRequested arm. CEF's message pump and the
            // render both run from the outer loop in `main`, never from inside
            // a winit callback. See `DemoApp::tick`.
            _ => {}
        }
        if close_requested {
            self.should_exit = true;
        }
    }
}

impl DemoApp {
    /// Finish CEF setup after `pump_app_events` has returned. CEF's macOS
    /// message pump drains the NSApplication queue, so this cannot happen in
    /// `resumed` without re-entering winit.
    fn start_pending(&mut self) {
        let Some(mut pending) = self.pending.take() else {
            return;
        };
        pending.cef_runtime.do_message_loop_work();
        let producer = match MacosCefProducer::try_new_with_prepared_profile(
            &pending.config,
            &mut pending.profile,
        ) {
            Ok(None) => {
                self.pending = Some(pending);
                return;
            }
            Ok(Some(producer)) => producer,
            Err(err) => {
                log::error!("failed to create CEF browser surface: {err}");
                self.should_exit = true;
                return;
            }
        };
        log::info!(
            "creating CEF browser ({}x{}) at {}",
            pending.config.surface.initial_size.width,
            pending.config.surface.initial_size.height,
            pending.config.surface.initial_url,
        );
        log::info!("CEF browser created");
        self.state = Some(DemoState {
            window: pending.window,
            surface: pending.surface,
            surface_config: pending.surface_config,
            host_ctx: pending.host_ctx,
            cef_runtime: pending.cef_runtime,
            producer,
            blit: pending.blit,
            frame: None,
            popup: None,
            frames_imported: 0,
            recover_url: pending.recover_url,
            battery_started: false,
            ticks: 0,
            cdp_ticks: 0,
            snapshot_requested: false,
            snapshot_path: pending.snapshot_path,
            popups_imported: 0,
            cursor: (0.0, 0.0),
            mods: EventModifiers::default(),
            started_at: Instant::now(),
            history_checked: false,
        });
    }

    /// One pass of CEF work plus one rendered frame, run *outside* winit's
    /// event dispatch.
    ///
    /// This split is not stylistic. On macOS `CefDoMessageLoopWork` drains the
    /// `NSApplication` event queue itself, so calling it from within a winit
    /// callback re-enters winit's handler and trips its re-entrancy guard:
    /// "tried to handle event while another event is currently being handled".
    /// Windows sidesteps this with CEF's own UI thread and Linux never had the
    /// problem, which is why only this demo is shaped that way.
    fn tick(&mut self) {
        let exit_after = self.exit_after_frames;
        if self.state.is_none() {
            self.start_pending();
        }
        let Some(s) = self.state.as_mut() else {
            return;
        };

        s.cef_runtime.do_message_loop_work();

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
            Err(e) => log::error!("acquire_frame error: {e}"),
        }

        // Popup widget surface. CEF paints this separately from the view, and
        // hides it without painting, so both the new-surface and the
        // still-open questions have to be asked every tick.
        match s.producer.acquire_popup(&s.host_ctx) {
            Ok(Some(popup)) => {
                s.popups_imported += 1;
                log::info!(
                    "imported popup #{} ({}x{} at {},{})",
                    s.popups_imported,
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

        if let Some(shape) = s.producer.poll_cursor_shape() {
            log::info!("cursor -> {shape:?}");
            s.window.set_cursor(present::winit_cursor(&shape));
        }

        while let Some(event) = s.producer.poll_navigation_event() {
            log::info!("nav: {event:?}");
            receipt(format_args!("nav: {event:?}"));
            scripted::recover_if_crashed(&mut s.producer, &s.recover_url, &event);
            scripted::answer_auth_if_challenged(&mut s.producer, &event);
            scripted::answer_permission_if_asked(&mut s.producer, &event);
            scripted::finish_page_drag_if_started(&mut s.producer, &event);
        }

        // Parity battery: one run reports frames, script results,
        // HiDPI layout and cookies, so the same evidence exists on
        // every platform.
        s.ticks += 1;
        if s.cdp_ticks == 200 || s.ticks == 200 {
            if let Ok(text) = std::env::var("WELD_FIND") {
                match s.producer.find(&text, true, false, false) {
                    Ok(()) => {
                        eprintln!("weld demo: find {text:?}");
                        receipt(format_args!("find requested: {text:?}"));
                    }
                    Err(e) => {
                        eprintln!("weld demo: find failed: {e}");
                        receipt(format_args!("find failed: {e}"));
                    }
                }
            }
            if let Ok(pdf) = std::env::var("WELD_PDF") {
                match s.producer.print_to_pdf(std::path::Path::new(&pdf)) {
                    Ok(()) => {
                        eprintln!("weld demo: print_to_pdf {pdf}");
                        receipt(format_args!("pdf requested: {pdf}"));
                    }
                    Err(e) => {
                        eprintln!("weld demo: print_to_pdf failed: {e}");
                        receipt(format_args!("pdf request failed: {e}"));
                    }
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
                receipt(format_args!("zoom: requested +2"));
            }
        }
        if !s.history_checked
            && std::env::var("WELD_HISTORY").is_ok()
            && s.started_at.elapsed() >= Duration::from_secs(2)
        {
            s.history_checked = true;
            let back = s.producer.can_go_back();
            let forward = s.producer.can_go_forward();
            eprintln!("weld demo: can_go_back={back} can_go_forward={forward}");
            receipt(format_args!(
                "history: can_go_back={back} can_go_forward={forward}"
            ));
        }
        // The normal snapshot is an early compositor receipt. A scripted
        // battery can opt into waiting until its last input has crossed into
        // CEF, so the pixels also record the final page-side gesture result.
        let wait_for_scripted_snapshot = std::env::var("WELD_SNAPSHOT_AFTER_SCRIPTED").is_ok();
        if !s.snapshot_requested
            && s.ticks > 90
            && s.snapshot_path.is_some()
            && (!wait_for_scripted_snapshot || self.scripted.complete())
        {
            s.snapshot_requested = true;
            match s.producer.request_snapshot_png() {
                Ok(()) => eprintln!("weld demo: PNG snapshot requested"),
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
        // Ticks, not imported frames: accelerated OSR only paints on change,
        // so a static page yields one frame and the battery would never fire.
        if !s.battery_started && s.ticks > 60 {
            s.battery_started = true;
            if let Ok(script) = std::env::var("WELD_SCRIPT") {
                match s.producer.request_script_result(&script) {
                    Ok(id) => log::info!("script request #{id}"),
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
                if let Err(e) = s.producer.request_cookies(Some(&url)) {
                    log::error!("request_cookies failed: {e}");
                }
            }
        }
        if let Some(result) = s.producer.poll_script_result() {
            match result.value {
                Ok(json) => {
                    log::info!("SCRIPT #{} => {json}", result.id);
                    receipt(format_args!("script #{}: {json}", result.id));
                }
                Err(err) => {
                    log::error!("SCRIPT #{} threw: {err}", result.id);
                    receipt(format_args!("script #{} failed: {err}", result.id));
                }
            }
        }
        if let Some(result) = s.producer.poll_snapshot_png() {
            match result {
                Ok(bytes) => {
                    if let Some(path) = s.snapshot_path.as_ref() {
                        match std::fs::write(path, &bytes) {
                            Ok(()) if bytes.starts_with(b"\x89PNG\r\n\x1a\n") => {
                                eprintln!(
                                    "weld demo: PNG snapshot {} bytes -> {}",
                                    bytes.len(),
                                    path.display()
                                );
                                receipt(format_args!("snapshot: {} bytes", bytes.len()));
                            }
                            Ok(()) => eprintln!("weld demo: snapshot was not a PNG"),
                            Err(e) => eprintln!("weld demo: could not write snapshot: {e}"),
                        }
                    }
                }
                Err(e) => eprintln!("weld demo: snapshot failed: {e}"),
            }
        }
        if let Some(cookies) = s.producer.poll_cookies() {
            log::info!("COOKIES n={}", cookies.len());
            for c in cookies.iter().take(3) {
                log::info!("  {}={} domain={}", c.name, c.value, c.domain);
            }
        }

        // The scripted gestures, for a machine nobody is sitting at. Only
        // once the page has painted: the first paint can land before its own
        // scripts and layout have settled.
        self.scripted.tick(&mut s.producer, s.frames_imported > 0);

        // With a scripted click the run has to outlive the frame that triggered
        // it, so the frame-count exit is replaced by a popup-count one.
        let done = if self.scripted.armed() {
            self.exit_after_popups
                .is_some_and(|n| s.popups_imported >= n)
        } else {
            exit_after.is_some_and(|n| s.frames_imported >= n)
        };
        if done {
            present::report(s);
            let _ = s.producer.close();
            self.should_exit = true;
            return;
        }

        present::render(s);
    }

    /// Report on whatever has been imported so far, for the timeout path.
    fn report_now(&mut self) {
        if let Some(s) = self.state.as_mut() {
            present::report(s);
            let _ = s.producer.close();
        }
    }
}

fn main() {
    // Unlike Windows and Linux, the main process must NOT call
    // cef_execute_process here. macOS spawns the separate Helper bundles
    // instead of re-executing this binary, and demo-weld-mac-helper is what
    // answers for them.
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let exe = std::env::current_exe().expect("current_exe failed");
    // welding joins the framework path onto what it is given, so hand it
    // Contents/Frameworks inside our own bundle:
    // <app>.app/Contents/MacOS/<exe> → <app>.app/Contents/Frameworks
    let frameworks = exe
        .parent()
        .and_then(|macos| macos.parent())
        .map(|contents| contents.join("Frameworks"))
        .expect("could not resolve Contents/Frameworks from the executable path");
    if !frameworks
        .join("Chromium Embedded Framework.framework")
        .exists()
    {
        eprintln!(
            "demo-weld-mac must run from its .app bundle; no framework at {}\n\
             Build it with: cd demo-weld-mac && cargo run --bin bundle-demo-weld-mac",
            frameworks.display()
        );
        std::process::exit(1);
    }
    log::info!("framework directory: {}", frameworks.display());

    let mut config = CefRuntimeConfig::new(&frameworks);
    // WELD_CACHE_ROOT is the CEF root cache. A WELD_PROFILE directory must
    // live inside it; this is CEF's process-wide RequestContext invariant.
    config.cache_path = std::env::var_os("WELD_CACHE_ROOT")
        .map(Into::into)
        .or_else(|| Some(std::env::temp_dir().join("welding-demo-mac-cache")));
    config.user_agent = std::env::var("WELD_UA").ok();
    config.user_agent_product = std::env::var("WELD_UA_PRODUCT").ok();
    // Never touch the login keychain: without this, Chromium's Safe Storage
    // asks for the user's password on launch — a modal prompt that blocks
    // CefInitialize for as long as nobody answers it, and crashes the network
    // service if answered Deny. A parity demo on a machine nobody sits at
    // cannot type a password.
    config
        .command_line_switches
        .push(("use-mock-keychain".to_owned(), None));
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
        eprintln!("weld demo: switches {:?}", config.command_line_switches);
    }
    let runtime = CefRuntime::initialize(config).expect("welding: CEF initialize failed");

    let exit_after_frames = std::env::var("WELD_EXIT_AFTER_FRAMES")
        .ok()
        .and_then(|v| v.parse::<u32>().ok());
    // An explicit WELD_TIMEOUT_SECS arms the timeout by itself. It used to
    // count only in unattended modes, so an interactive run with a timeout
    // set ran forever — which read as a post-crash hang the first time a
    // crash test relied on it.
    let timeout_explicit = std::env::var("WELD_TIMEOUT_SECS").is_ok();
    let timeout = Duration::from_secs(
        std::env::var("WELD_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(60),
    );
    if let Some(n) = exit_after_frames {
        log::info!(
            "unattended mode: will exit after {n} imported frames or {}s",
            timeout.as_secs()
        );
    }

    let mut event_loop = EventLoop::new().expect("event loop creation failed");
    event_loop.set_control_flow(ControlFlow::Poll);

    let scripted = scripted::ScriptedInput::from_env();
    let exit_after_popups = std::env::var("WELD_EXIT_AFTER_POPUPS")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .or(scripted.armed().then_some(1));

    let scripted_armed = scripted.armed();
    let mut app = DemoApp {
        cef_runtime: Some(runtime),
        pending: None,
        state: None,
        exit_after_frames,
        exit_after_popups,
        should_exit: false,
        scripted,
    };

    // We drive the loop rather than handing it to `run_app`, so that CEF's
    // pump and our rendering happen between winit dispatches instead of inside
    // one. `Duration::ZERO` makes each pump non-blocking.
    let started = Instant::now();
    loop {
        if let PumpStatus::Exit(code) = event_loop.pump_app_events(Some(Duration::ZERO), &mut app) {
            log::info!("winit asked to exit ({code})");
            break;
        }
        app.tick();
        if app.should_exit {
            break;
        }
        if (exit_after_frames.is_some() || scripted_armed || timeout_explicit)
            && started.elapsed() >= timeout
        {
            log::warn!("timed out after {}s", timeout.as_secs());
            app.report_now();
            break;
        }
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

/// `WELD_RECEIPT=/path/to/file` appends validation lines for a GUI launch
/// where macOS has no useful stdout sink.
fn receipt(line: std::fmt::Arguments<'_>) {
    let Some(path) = std::env::var_os("WELD_RECEIPT") else {
        return;
    };
    let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    else {
        return;
    };
    let _ = writeln!(file, "{line}");
}
