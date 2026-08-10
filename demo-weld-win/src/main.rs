//! Windows demo: CEF accelerated OSR → wgpu surface via `welding`.
//!
//! # Running
//!
//! ```text
//! set CEF_PATH=C:\path\to\cef_binary_148.x_windows64
//! cargo run -p demo-weld-win
//! ```

use std::sync::Arc;

use winit::{
    application::ApplicationHandler,
    dpi::PhysicalSize,
    event::{ElementState, MouseButton as WinitMouseButton, MouseScrollDelta, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    keyboard::PhysicalKey,
    window::{Window, WindowAttributes},
};

use welding::{
    windows_cef::{WindowsCefConfig, WindowsCefProducer},
    CefRuntime, CefRuntimeConfig, CefSurfaceConfig, CefSurfaceProducer, EventModifiers,
    FocusDirection, HostWgpuContext, ImportedTexture, KeyEvent, KeyEventKind, MouseAction,
    MouseButton, MouseEvent, PopupSurface,
};

mod blit;
mod keys;

use crate::{blit::build_blit_pipeline, keys::keycode_to_vk};

// ── App state ─────────────────────────────────────────────────────────────────

struct DemoApp {
    cef_runtime: Option<CefRuntime>,
    state: Option<DemoState>,
}

struct DemoState {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    surface_config: wgpu::SurfaceConfiguration,
    host_ctx: HostWgpuContext,
    producer: WindowsCefProducer,
    _cef_runtime: CefRuntime,
    pipeline: wgpu::RenderPipeline,
    bg_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    frame: Option<ImportedTexture>,
    /// Cached popup widget surface, held across frames because CEF only
    /// repaints it on change and dropped when `popup_rect` goes to `None`.
    popup: Option<PopupSurface>,
    frames_drawn: u32,
    click_at: Option<(i32, i32)>,
    clicked: bool,
    cursor: (f32, f32),
    mods: EventModifiers,
    closing: bool,
}

impl DemoApp {
    fn new(cef_runtime: CefRuntime) -> Self {
        Self { cef_runtime: Some(cef_runtime), state: None }
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
                    .with_title("weld demo — CEF → wgpu (Dx12)")
                    .with_inner_size(PhysicalSize::new(1280u32, 800u32)),
            )
            .expect("window creation failed"),
        );

        let (device, queue, surface, surface_config) = pollster::block_on(async {
            let mut instance_desc = wgpu::InstanceDescriptor::new_without_display_handle();
            instance_desc.backends = wgpu::Backends::DX12;
            let instance = wgpu::Instance::new(instance_desc);
            let surface = instance.create_surface(window.clone()).unwrap();
            let adapter = instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::HighPerformance,
                    compatible_surface: Some(&surface),
                    force_fallback_adapter: false,
                })
                .await
                .expect("no suitable wgpu adapter (need Dx12)");
            let (device, queue) = adapter
                .request_device(&wgpu::DeviceDescriptor {
                    label: Some("weld-demo"),
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
        let (pipeline, bg_layout, sampler) =
            build_blit_pipeline(&host_ctx.device, surface_config.format);

        let cef_runtime = self.cef_runtime.take().unwrap();
        let win_size = window.inner_size();
        let initial_url =
            std::env::var("WELD_URL").unwrap_or_else(|_| "https://example.com".into());
        let producer = WindowsCefProducer::new(
            &cef_runtime,
            WindowsCefConfig {
                surface: CefSurfaceConfig {
                    initial_url: initial_url.clone(),
                    initial_size: win_size,
                    ..Default::default()
                },
            },
            &host_ctx,
        )
        .expect("failed to create CEF browser surface");

        self.state = Some(DemoState {
            window,
            surface,
            surface_config,
            host_ctx,
            _cef_runtime: cef_runtime,
            producer,
            pipeline,
            bg_layout,
            sampler,
            frame: None,
            popup: None,
            frames_drawn: 0,
            click_at: std::env::var("WELD_CLICK_AT").ok().and_then(|v| {
                let (x, y) = v.split_once(',')?;
                Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
            }),
            clicked: false,
            cursor: (0.0, 0.0),
            mods: EventModifiers::default(),
            closing: false,
        });
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
                if !s.closing {
                    eprintln!("weld demo: close requested");
                    s.closing = true;
                    if let Err(err) = s.producer.close() {
                        eprintln!("weld demo: close failed: {err}");
                        el.exit();
                    } else {
                        s.window.request_redraw();
                    }
                }
            }

            WindowEvent::Resized(size) => {
                s.surface_config.width = size.width.max(1);
                s.surface_config.height = size.height.max(1);
                s.surface.configure(&s.host_ctx.device, &s.surface_config);
                if let Err(err) = s.producer.resize(size) {
                    eprintln!("weld demo: resize failed: {err}");
                }
            }

            WindowEvent::Focused(true) => {
                if let Err(err) = s.producer.move_focus(FocusDirection::Forward) {
                    eprintln!("weld demo: move_focus failed: {err}");
                }
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
                    // RawKeyDown first (non-printable nav/modifier keys rely on this)
                    if let Err(err) = s.producer.send_keyboard_input(KeyEvent {
                        kind: KeyEventKind::RawKeyDown,
                        windows_key_code: vk,
                        native_key_code: 0,
                        character: None,
                        modifiers: s.mods,
                    }) {
                        eprintln!("weld demo: send RawKeyDown failed: {err}");
                    }
                    // Char for printable text — winit's .text field includes Shift
                    if let Some(ch) =
                        ke.text.as_ref().and_then(|t| t.chars().next())
                    {
                        if let Err(err) = s.producer.send_keyboard_input(KeyEvent {
                            kind: KeyEventKind::Char,
                            windows_key_code: vk,
                            native_key_code: 0,
                            character: Some(ch),
                            modifiers: s.mods,
                        }) {
                            eprintln!("weld demo: send Char failed: {err}");
                        }
                    }
                } else {
                    if let Err(err) = s.producer.send_keyboard_input(KeyEvent {
                        kind: KeyEventKind::KeyUp,
                        windows_key_code: vk,
                        native_key_code: 0,
                        character: None,
                        modifiers: s.mods,
                    }) {
                        eprintln!("weld demo: send KeyUp failed: {err}");
                    }
                }
            }

            WindowEvent::CursorMoved { position, .. } => {
                s.cursor = (position.x as f32, position.y as f32);
                if let Err(err) = s.producer.send_mouse_input(MouseEvent {
                    x: position.x as i32,
                    y: position.y as i32,
                    button: MouseButton::Left,
                    action: MouseAction::Moved,
                    modifiers: s.mods,
                }) {
                    eprintln!("weld demo: mouse move failed: {err}");
                }
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
                if let Err(err) = s.producer.send_mouse_input(MouseEvent {
                    x: s.cursor.0 as i32,
                    y: s.cursor.1 as i32,
                    button: mb,
                    action,
                    modifiers: s.mods,
                }) {
                    eprintln!("weld demo: mouse button failed: {err}");
                }
            }

            WindowEvent::MouseWheel { delta, .. } => {
                let (dx, dy) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => {
                        ((x * 20.0) as i32, (y * 20.0) as i32)
                    }
                    MouseScrollDelta::PixelDelta(d) => (d.x as i32, d.y as i32),
                };
                if let Err(err) = s.producer.send_mouse_input(MouseEvent {
                    x: s.cursor.0 as i32,
                    y: s.cursor.1 as i32,
                    button: MouseButton::Left,
                    action: MouseAction::WheelScrolled { delta_x: dx, delta_y: dy },
                    modifiers: s.mods,
                }) {
                    eprintln!("weld demo: mouse wheel failed: {err}");
                }
            }

            WindowEvent::RedrawRequested => {
                match s.producer.acquire_frame(&s.host_ctx) {
                    Ok(Some(new_frame)) => {
                        s.frame = Some(new_frame);
                    }
                    Ok(None) => {}
                    Err(err) => {
                        eprintln!("weld demo: acquire_frame failed: {err}");
                    }
                }

                // Popup widget surface. CEF paints it separately from the view
                // and hides it without painting, so both questions get asked
                // every frame.
                match s.producer.acquire_popup(&s.host_ctx) {
                    Ok(Some(popup)) => {
                        eprintln!(
                            "weld demo: imported popup {}x{} at {},{}",
                            popup.rect.width, popup.rect.height, popup.rect.x, popup.rect.y
                        );
                        s.popup = Some(popup);
                    }
                    Ok(None) => {}
                    Err(err) => eprintln!("weld demo: acquire_popup failed: {err}"),
                }
                if s.producer.popup_rect().is_none() && s.popup.take().is_some() {
                    eprintln!("weld demo: popup closed");
                }

                while let Some(event) = s.producer.poll_navigation_event() {
                    eprintln!("weld demo: navigation event: {event:?}");
                }

                // WELD_CLICK_AT=x,y clicks once, a few frames in. A <select>
                // dropdown needs a real gesture, so without this the popup path
                // cannot be exercised without a human at the keyboard.
                s.frames_drawn += 1;
                if let Some((cx, cy)) = s.click_at {
                    if !s.clicked && s.frames_drawn > 120 {
                        s.clicked = true;
                        eprintln!("weld demo: scripted click at {cx},{cy}");
                        for action in
                            [MouseAction::Moved, MouseAction::Pressed, MouseAction::Released]
                        {
                            let _ = s.producer.send_mouse_input(MouseEvent {
                                x: cx,
                                y: cy,
                                button: MouseButton::Left,
                                action,
                                modifiers: EventModifiers::default(),
                            });
                        }
                    }
                }

                let output = match s.surface.get_current_texture() {
                    wgpu::CurrentSurfaceTexture::Success(t)
                    | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
                    wgpu::CurrentSurfaceTexture::Lost
                    | wgpu::CurrentSurfaceTexture::Outdated => {
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
                        eprintln!("weld demo: surface validation error");
                        return;
                    }
                };

                let target = output.texture.create_view(&wgpu::TextureViewDescriptor::default());
                let mut enc = s.host_ctx.device.create_command_encoder(
                    &wgpu::CommandEncoderDescriptor { label: Some("blit") },
                );

                let make_bg = |view: &wgpu::TextureView| {
                    s.host_ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
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
                        wgpu::Color { r: 0.1, g: 0.1, b: 0.1, a: 1.0 }
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
                output.present();
                if s.closing && s.producer.is_closed() {
                    el.exit();
                } else {
                    s.window.request_redraw();
                }
            }

            _ => {}
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        let Some(s) = self.state.as_mut() else {
            return;
        };

        if s.closing {
            if s.producer.is_closed() {
                eprintln!("weld demo: CEF browser closed; exiting event loop");
                el.exit();
            } else {
                s.window.request_redraw();
            }
        } else {
            s.window.request_redraw();
        }
    }
}


// ── main ──────────────────────────────────────────────────────────────────────

fn main() {
    // MUST be first: CEF re-invokes this binary for renderer/GPU/utility subprocesses.
    let cef_path = std::env::var("CEF_PATH")
        .expect("CEF_PATH must point to the CEF binary distribution (contains libcef.dll)");
    if let Some(code) = CefRuntime::execute_process_from(cef_path.as_ref())
        .expect("weld: CEF subprocess probe failed — is CEF_PATH set correctly?")
    {
        std::process::exit(code);
    }

    let mut runtime_config = CefRuntimeConfig::new(&cef_path);
    runtime_config.cache_path = Some(std::env::temp_dir().join("wgpu-weld-demo-cache"));
    let runtime = CefRuntime::initialize(runtime_config)
        .expect("weld: CEF initialize failed");

    let event_loop = EventLoop::new().expect("event loop creation failed");
    event_loop.set_control_flow(ControlFlow::Poll);

    event_loop
        .run_app(&mut DemoApp::new(runtime))
        .expect("event loop error");
}
