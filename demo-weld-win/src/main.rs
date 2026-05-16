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
    keyboard::{KeyCode, PhysicalKey},
    window::{Window, WindowAttributes},
};

use welding::{
    windows_cef::{WindowsCefConfig, WindowsCefProducer},
    CefRuntime, CefRuntimeConfig, CefSurfaceConfig, CefSurfaceProducer, EventModifiers,
    FocusDirection, HostWgpuContext, ImportedTexture, KeyEvent, KeyEventKind, MouseAction,
    MouseButton, MouseEvent,
};

// ── Blit shader: full-screen triangle that samples the CEF texture ────────────

const BLIT_WGSL: &str = "
struct VOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> }

@vertex fn vs(@builtin(vertex_index) vi: u32) -> VOut {
    var p = array<vec2<f32>,3>(vec2(-1.,-1.), vec2(3.,-1.), vec2(-1.,3.));
    var u = array<vec2<f32>,3>(vec2(0.,1.),  vec2(2.,1.),  vec2(0.,-1.));
    return VOut(vec4<f32>(p[vi], 0., 1.), u[vi]);
}

@group(0) @binding(0) var t: texture_2d<f32>;
@group(0) @binding(1) var s: sampler;

@fragment fn fs(v: VOut) -> @location(0) vec4<f32> {
    return textureSample(t, s, v.uv);
}";

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
    cef_runtime: CefRuntime,
    producer: WindowsCefProducer,
    pipeline: wgpu::RenderPipeline,
    bg_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    frame: Option<ImportedTexture>,
    cursor: (f32, f32),
    mods: EventModifiers,
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
            let instance = wgpu::Instance::default();
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
                .request_device(
                    &wgpu::DeviceDescriptor {
                        label: Some("weld-demo"),
                        required_features: wgpu::Features::empty(),
                        required_limits: wgpu::Limits::default(),
                        memory_hints: wgpu::MemoryHints::default(),
                    },
                    None,
                )
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
        let producer = WindowsCefProducer::new(
            &cef_runtime,
            WindowsCefConfig {
                surface: CefSurfaceConfig {
                    initial_url: "https://example.com".into(),
                    initial_size: win_size,
                    ..Default::default()
                },
            },
        )
        .expect("failed to create CEF browser surface");

        self.state = Some(DemoState {
            window,
            surface,
            surface_config,
            host_ctx,
            cef_runtime,
            producer,
            pipeline,
            bg_layout,
            sampler,
            frame: None,
            cursor: (0.0, 0.0),
            mods: EventModifiers::default(),
        });
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
                    // RawKeyDown first (non-printable nav/modifier keys rely on this)
                    let _ = s.producer.send_keyboard_input(KeyEvent {
                        kind: KeyEventKind::RawKeyDown,
                        windows_key_code: vk,
                        native_key_code: 0,
                        character: None,
                        modifiers: s.mods,
                    });
                    // Char for printable text — winit's .text field includes Shift
                    if let Some(ch) =
                        ke.text.as_ref().and_then(|t| t.chars().next())
                    {
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
                    MouseScrollDelta::LineDelta(x, y) => {
                        ((x * 20.0) as i32, (y * 20.0) as i32)
                    }
                    MouseScrollDelta::PixelDelta(d) => (d.x as i32, d.y as i32),
                };
                let _ = s.producer.send_mouse_input(MouseEvent {
                    x: s.cursor.0 as i32,
                    y: s.cursor.1 as i32,
                    button: MouseButton::Left,
                    action: MouseAction::WheelScrolled { delta_x: dx, delta_y: dy },
                    modifiers: s.mods,
                });
            }

            WindowEvent::RedrawRequested => {
                s.cef_runtime.do_message_loop_work();

                if let Ok(Some(new_frame)) = s.producer.acquire_frame(&s.host_ctx) {
                    s.frame = Some(new_frame);
                }

                let output = match s.surface.get_current_texture() {
                    Ok(t) => t,
                    Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                        s.surface.configure(&s.host_ctx.device, &s.surface_config);
                        s.window.request_redraw();
                        return;
                    }
                    Err(e) => {
                        eprintln!("weld demo: surface error: {e}");
                        return;
                    }
                };

                let target = output.texture.create_view(&wgpu::TextureViewDescriptor::default());
                let mut enc = s.host_ctx.device.create_command_encoder(
                    &wgpu::CommandEncoderDescriptor { label: Some("blit") },
                );

                let bg = s.frame.as_ref().map(|f| {
                    s.host_ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: None,
                        layout: &s.bg_layout,
                        entries: &[
                            wgpu::BindGroupEntry {
                                binding: 0,
                                resource: wgpu::BindingResource::TextureView(&f.view),
                            },
                            wgpu::BindGroupEntry {
                                binding: 1,
                                resource: wgpu::BindingResource::Sampler(&s.sampler),
                            },
                        ],
                    })
                });

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
                }

                s.host_ctx.queue.submit([enc.finish()]);
                output.present();
                s.window.request_redraw();
            }

            _ => {}
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn build_blit_pipeline(
    device: &wgpu::Device,
    fmt: wgpu::TextureFormat,
) -> (wgpu::RenderPipeline, wgpu::BindGroupLayout, wgpu::Sampler) {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("blit"),
        source: wgpu::ShaderSource::Wgsl(BLIT_WGSL.into()),
    });
    let bg_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("blit-bgl"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    multisampled: false,
                    view_dimension: wgpu::TextureViewDimension::D2,
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("blit-layout"),
        bind_group_layouts: &[&bg_layout],
        push_constant_ranges: &[],
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("blit"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: fmt,
                blend: Some(wgpu::BlendState::REPLACE),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview: None,
        cache: None,
    });
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("blit-sampler"),
        min_filter: wgpu::FilterMode::Linear,
        mag_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });
    (pipeline, bg_layout, sampler)
}

fn keycode_to_vk(kc: KeyCode) -> i32 {
    match kc {
        KeyCode::Backspace                             => 0x08,
        KeyCode::Tab                                   => 0x09,
        KeyCode::Enter                                 => 0x0D,
        KeyCode::ShiftLeft   | KeyCode::ShiftRight     => 0x10,
        KeyCode::ControlLeft | KeyCode::ControlRight   => 0x11,
        KeyCode::AltLeft     | KeyCode::AltRight       => 0x12,
        KeyCode::Escape                                => 0x1B,
        KeyCode::Space                                 => 0x20,
        KeyCode::PageUp                                => 0x21,
        KeyCode::PageDown                              => 0x22,
        KeyCode::End                                   => 0x23,
        KeyCode::Home                                  => 0x24,
        KeyCode::ArrowLeft                             => 0x25,
        KeyCode::ArrowUp                               => 0x26,
        KeyCode::ArrowRight                            => 0x27,
        KeyCode::ArrowDown                             => 0x28,
        KeyCode::Delete                                => 0x2E,
        KeyCode::Digit0 => 0x30, KeyCode::Digit1 => 0x31, KeyCode::Digit2 => 0x32,
        KeyCode::Digit3 => 0x33, KeyCode::Digit4 => 0x34, KeyCode::Digit5 => 0x35,
        KeyCode::Digit6 => 0x36, KeyCode::Digit7 => 0x37, KeyCode::Digit8 => 0x38,
        KeyCode::Digit9 => 0x39,
        KeyCode::KeyA => 0x41, KeyCode::KeyB => 0x42, KeyCode::KeyC => 0x43,
        KeyCode::KeyD => 0x44, KeyCode::KeyE => 0x45, KeyCode::KeyF => 0x46,
        KeyCode::KeyG => 0x47, KeyCode::KeyH => 0x48, KeyCode::KeyI => 0x49,
        KeyCode::KeyJ => 0x4A, KeyCode::KeyK => 0x4B, KeyCode::KeyL => 0x4C,
        KeyCode::KeyM => 0x4D, KeyCode::KeyN => 0x4E, KeyCode::KeyO => 0x4F,
        KeyCode::KeyP => 0x50, KeyCode::KeyQ => 0x51, KeyCode::KeyR => 0x52,
        KeyCode::KeyS => 0x53, KeyCode::KeyT => 0x54, KeyCode::KeyU => 0x55,
        KeyCode::KeyV => 0x56, KeyCode::KeyW => 0x57, KeyCode::KeyX => 0x58,
        KeyCode::KeyY => 0x59, KeyCode::KeyZ => 0x5A,
        KeyCode::F1  => 0x70, KeyCode::F2  => 0x71, KeyCode::F3  => 0x72,
        KeyCode::F4  => 0x73, KeyCode::F5  => 0x74, KeyCode::F6  => 0x75,
        KeyCode::F7  => 0x76, KeyCode::F8  => 0x77, KeyCode::F9  => 0x78,
        KeyCode::F10 => 0x79, KeyCode::F11 => 0x7A, KeyCode::F12 => 0x7B,
        _ => 0,
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

    let runtime = CefRuntime::initialize(CefRuntimeConfig::new(&cef_path))
        .expect("weld: CEF initialize failed");

    let event_loop = EventLoop::new().expect("event loop creation failed");
    event_loop.set_control_flow(ControlFlow::Poll);

    event_loop
        .run_app(&mut DemoApp::new(runtime))
        .expect("event loop error");
}
