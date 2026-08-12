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
    platform::pump_events::{EventLoopExtPumpEvents, PumpStatus},
    window::{Window, WindowAttributes},
};

use welding::{
    macos_cef::{MacosCefConfig, MacosCefProducer},
    CefRuntime, CefRuntimeConfig, CefSurfaceConfig, CefSurfaceProducer, EventModifiers,
    FocusDirection, HostWgpuContext, ImportedTexture, KeyEvent, KeyEventKind, MouseAction,
    MouseButton, MouseEvent, PopupSurface,
};

use crate::keys::keycode_to_vk;

struct DemoApp {
    cef_runtime: Option<CefRuntime>,
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
    click_at: Option<(i32, i32)>,
    ticks_since_first_frame: u32,
    clicked: bool,
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
    popups_imported: u32,
    cursor: (f32, f32),
    mods: EventModifiers,
}

impl ApplicationHandler for DemoApp {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.state.is_some() {
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
        let scale = std::env::var("WELD_SCALE")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or_else(|| window.scale_factor());
        let url = std::env::var("WELD_URL").unwrap_or_else(|_| "https://example.com".into());
        log::info!("creating CEF browser ({}x{}) at {url}", win_size.width, win_size.height);
        let producer = MacosCefProducer::new(
            &cef_runtime,
            MacosCefConfig {
                surface: CefSurfaceConfig {
                    initial_url: url,
                    initial_size: win_size,
                    // Physical size plus the display scale: CEF lays out at
                    // size/scale CSS pixels and paints the full physical size.
                    scale_factor: scale as f32,
                    ..Default::default()
                },
            },
        )
        .expect("failed to create CEF browser surface");
        log::info!("CEF browser created");

        self.state = Some(DemoState {
            window,
            surface,
            surface_config,
            host_ctx,
            cef_runtime,
            producer,
            blit,
            frame: None,
            popup: None,
            frames_imported: 0,
            popups_imported: 0,
            cursor: (0.0, 0.0),
            mods: EventModifiers::default(),
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
                if let Err(err) = s.producer.set_scale_factor(scale_factor as f32) {
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
                s.mods = EventModifiers {
                    shift: m.state().shift_key(),
                    ctrl: m.state().control_key(),
                    alt: m.state().alt_key(),
                    meta: m.state().super_key(),
                };
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
        }

        if let Some((x, y)) = self.click_at {
            if s.frames_imported > 0 {
                self.ticks_since_first_frame += 1;
                // A few ticks of slack: the first paint can land before the
                // page's own scripts and layout have settled.
                if !self.clicked && self.ticks_since_first_frame > 30 {
                    self.clicked = true;
                    log::info!("scripted click at {x},{y}");
                    for action in [MouseAction::Moved, MouseAction::Pressed, MouseAction::Released]
                    {
                        let _ = s.producer.send_mouse_input(MouseEvent {
                            x,
                            y,
                            button: MouseButton::Left,
                            action,
                            modifiers: EventModifiers::default(),
                        });
                    }
                }
            }
        }

        // With a scripted click the run has to outlive the frame that triggered
        // it, so the frame-count exit is replaced by a popup-count one.
        let done = if self.click_at.is_some() {
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
    if !frameworks.join("Chromium Embedded Framework.framework").exists() {
        eprintln!(
            "demo-weld-mac must run from its .app bundle; no framework at {}\n\
             Build it with: cd demo-weld-mac && cargo run --bin bundle-demo-weld-mac",
            frameworks.display()
        );
        std::process::exit(1);
    }
    log::info!("framework directory: {}", frameworks.display());

    let mut config = CefRuntimeConfig::new(&frameworks);
    config.cache_path = Some(std::env::temp_dir().join("welding-demo-mac-cache"));
    let runtime = CefRuntime::initialize(config).expect("welding: CEF initialize failed");

    let exit_after_frames = std::env::var("WELD_EXIT_AFTER_FRAMES")
        .ok()
        .and_then(|v| v.parse::<u32>().ok());
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

    let click_at = std::env::var("WELD_CLICK_AT").ok().and_then(|v| {
        let (x, y) = v.split_once(',')?;
        Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
    });
    let exit_after_popups = std::env::var("WELD_EXIT_AFTER_POPUPS")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .or(click_at.map(|_| 1));
    if let Some((x, y)) = click_at {
        log::info!("scripted click armed at {x},{y}");
    }

    let mut app = DemoApp {
        cef_runtime: Some(runtime),
        state: None,
        exit_after_frames,
        exit_after_popups,
        should_exit: false,
        click_at,
        ticks_since_first_frame: 0,
        clicked: false,
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
        if (exit_after_frames.is_some() || click_at.is_some()) && started.elapsed() >= timeout {
            log::warn!("timed out after {}s", timeout.as_secs());
            app.report_now();
            break;
        }
    }
}

fn log_scale_err(err: welding::WeldError) {
    log::error!("set_scale_factor failed: {err}");
}
