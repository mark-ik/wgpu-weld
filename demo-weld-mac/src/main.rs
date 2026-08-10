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
    MouseButton, MouseEvent,
};

use crate::keys::keycode_to_vk;

struct DemoApp {
    cef_runtime: Option<CefRuntime>,
    state: Option<DemoState>,
    exit_after_frames: Option<u32>,
    should_exit: bool,
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
    frames_imported: u32,
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
        let url = std::env::var("WELD_URL").unwrap_or_else(|_| "https://example.com".into());
        log::info!("creating CEF browser ({}x{}) at {url}", win_size.width, win_size.height);
        let producer = MacosCefProducer::new(
            &cef_runtime,
            MacosCefConfig {
                surface: CefSurfaceConfig {
                    initial_url: url,
                    initial_size: win_size,
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
            frames_imported: 0,
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

        if let Some(limit) = exit_after {
            if s.frames_imported >= limit {
                report(s);
                let _ = s.producer.close();
                self.should_exit = true;
                return;
            }
        }

        render(s);
    }

    /// Report on whatever has been imported so far, for the timeout path.
    fn report_now(&mut self) {
        if let Some(s) = self.state.as_mut() {
            report(s);
            let _ = s.producer.close();
        }
    }
}

fn render(s: &mut DemoState) {
    let output = match s.surface.get_current_texture() {
        wgpu::CurrentSurfaceTexture::Success(t) | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
        wgpu::CurrentSurfaceTexture::Lost | wgpu::CurrentSurfaceTexture::Outdated => {
            s.surface.configure(&s.host_ctx.device, &s.surface_config);
            s.window.request_redraw();
            return;
        }
        wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
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
    let mut enc = s
        .host_ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("blit"),
        });

    let bg = s.frame.as_ref().map(|f| {
        s.host_ctx
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &s.blit.bg_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&f.view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&s.blit.sampler),
                    },
                ],
            })
    });

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
            rpass.set_pipeline(&s.blit.pipeline);
            rpass.set_bind_group(0, bg, &[]);
            rpass.draw(0..3, 0..1);
        }
    }

    s.host_ctx.queue.submit([enc.finish()]);
    output.present();
    s.window.request_redraw();
}

/// Probe the last imported frame and print a verdict a log reader can trust.
fn report(s: &mut DemoState) {
    match s.frame.as_ref() {
        Some(frame) => match probe::sample(&s.host_ctx.device, &s.host_ctx.queue, &frame.texture) {
            Ok(rb) => {
                log::info!(
                    "probe: {}/{} bytes non-zero in the top-left corner; first pixels {:?}",
                    rb.non_zero_bytes,
                    rb.total_bytes,
                    rb.first_pixels
                );
                if rb.looks_painted() {
                    log::info!(
                        "VALIDATION PASS: {} frames imported and the IOSurface carried real pixels",
                        s.frames_imported
                    );
                } else {
                    log::error!(
                        "VALIDATION FAIL: {} frames imported but the corner is entirely zero, \
                         so the texture is not carrying CEF's paint",
                        s.frames_imported
                    );
                }
            }
            Err(e) => log::error!("VALIDATION FAIL: readback failed: {e}"),
        },
        None => log::error!("VALIDATION FAIL: no frame was ever imported"),
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

    let mut app = DemoApp {
        cef_runtime: Some(runtime),
        state: None,
        exit_after_frames,
        should_exit: false,
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
        if exit_after_frames.is_some() && started.elapsed() >= timeout {
            log::warn!("timed out after {}s", timeout.as_secs());
            app.report_now();
            break;
        }
    }
}
