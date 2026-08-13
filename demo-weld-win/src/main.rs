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
mod scripted;

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
    /// Where a crashed renderer is sent back to; see the recovery path below.
    recover_url: String,
    frames_imported: u32,
    _cef_runtime: CefRuntime,
    pipeline: wgpu::RenderPipeline,
    bg_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    frame: Option<ImportedTexture>,
    /// Cached popup widget surface, held across frames because CEF only
    /// repaints it on change and dropped when `popup_rect` goes to `None`.
    popup: Option<PopupSurface>,
    frames_drawn: u32,
    cookies_requested: bool,
    script_requested: bool,
    scripted: scripted::ScriptedInput,
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
        // WELD_SCALE forces a scale factor regardless of the display, which is
        // how the HiDPI path gets exercised on a 1x screen.
        let scale = forced_scale().unwrap_or_else(|| window.scale_factor());
        let initial_url =
            std::env::var("WELD_URL").unwrap_or_else(|_| "https://example.com".into());
        let producer = WindowsCefProducer::new(
            &cef_runtime,
            WindowsCefConfig {
                surface: CefSurfaceConfig {
                    initial_url: initial_url.clone(),
                    initial_size: win_size,
                    // Physical size plus the display scale: CEF lays out at
                    // size/scale CSS pixels and paints the full physical size.
                    scale_factor: scale as f32,
                    background_color: env_background(),
                    // WELD_DOWNLOAD_DIR=path accepts downloads into that
                    // directory; unset refuses them, which is the default.
                    download_dir: std::env::var("WELD_DOWNLOAD_DIR").ok().map(Into::into),
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
            recover_url: initial_url,
            frames_imported: 0,
            pipeline,
            bg_layout,
            sampler,
            frame: None,
            popup: None,
            frames_drawn: 0,
            cookies_requested: false,
            script_requested: false,
            scripted: scripted::ScriptedInput::from_env(),
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

            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                // The window crossed onto a display with a different density.
                // A forced WELD_SCALE outranks it: on a 1x panel winit reports
                // 1.0 here and would undo the very override being tested.
                if forced_scale().is_some() {
                    log::info!("ScaleFactorChanged({scale_factor}) ignored: WELD_SCALE pins the scale");
                } else if let Err(err) = s.producer.set_scale_factor(scale_factor as f32) {
                    log_scale_err(err);
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
                        s.frames_imported += 1;
                        eprintln!(
                            "weld demo: imported frame #{} ({}x{} {:?})",
                            s.frames_imported,
                            new_frame.size.width,
                            new_frame.size.height,
                            new_frame.format
                        );
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

                // The page owns the cursor's meaning; the host owns the
                // pointer. Nothing shows an I-beam unless we apply it.
                if let Some(shape) = s.producer.poll_cursor_shape() {
                    eprintln!("weld demo: cursor -> {shape:?}");
                    s.window.set_cursor(winit_cursor(&shape));
                }

                // WELD_COOKIE_URL: request the store once a page has loaded,
                // then print whatever comes back. Proves the async read.
                if !s.cookies_requested && s.frames_drawn > 90 {
                    if let Ok(url) = std::env::var("WELD_COOKIE_URL") {
                        s.cookies_requested = true;
                        // Write one first, so the read has something to find
                        // even on a site that sets none.
                        let probe = welding::Cookie {
                            name: "weld_probe".into(),
                            value: "w5".into(),
                            domain: "example.com".into(),
                            path: "/".into(),
                            ..Default::default()
                        };
                        match s.producer.set_cookie(&url, &probe) {
                            Ok(()) => eprintln!("weld demo: set_cookie accepted"),
                            Err(e) => eprintln!("weld demo: set_cookie failed: {e}"),
                        }
                        match s.producer.request_cookies(Some(&url)) {
                            Ok(()) => eprintln!("weld demo: requested cookies for {url}"),
                            Err(e) => eprintln!("weld demo: request_cookies failed: {e}"),
                        }
                    }
                }
                // WELD_SCRIPT: evaluate once the page is up, then print the
                // value the renderer sent back.
                if !s.script_requested && s.frames_drawn > 90 {
                    if let Ok(script) = std::env::var("WELD_SCRIPT") {
                        s.script_requested = true;
                        match s.producer.request_script_result(&script) {
                            Ok(id) => eprintln!("weld demo: script request #{id}: {script}"),
                            Err(e) => eprintln!("weld demo: request_script_result failed: {e}"),
                        }
                    }
                }
                if let Some(result) = s.producer.poll_script_result() {
                    match result.value {
                        Ok(json) => eprintln!("weld demo: SCRIPT #{} => {json}", result.id),
                        Err(err) => eprintln!("weld demo: SCRIPT #{} threw: {err}", result.id),
                    }
                }

                if let Some(cookies) = s.producer.poll_cookies() {
                    eprintln!("weld demo: COOKIES n={}", cookies.len());
                    for c in cookies.iter().take(5) {
                        eprintln!(
                            "weld demo:   {}={} domain={} path={} secure={} http_only={}",
                            c.name, c.value, c.domain, c.path, c.secure, c.http_only
                        );
                    }
                }

                while let Some(event) = s.producer.poll_navigation_event() {
                    eprintln!("weld demo: navigation event: {event:?}");
                    scripted::recover_if_crashed(
                        &mut s.producer,
                        &s.recover_url,
                        &event,
                    );
                }

                // The scripted gestures, for a machine nobody is sitting at:
                // a <select> dropdown needs a real click, and a key never
                // reaches the DOM without a real key event.
                s.frames_drawn += 1;
                s.scripted.tick(&mut s.producer, true);

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

    // After the fork-guard, so CEF's renderer/GPU helpers do not each stand up
    // their own logger. Without this, welding's own log:: output is invisible,
    // which is exactly the wrong thing for a reference demo.
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let mut runtime_config = CefRuntimeConfig::new(&cef_path);
    // WELD_SWITCHES=disable-popup-blocking,lang=en-GB
    if let Ok(list) = std::env::var("WELD_SWITCHES") {
        runtime_config.command_line_switches = list
            .split(',')
            .filter(|s| !s.is_empty())
            .map(|s| match s.split_once('=') {
                Some((k, v)) => (k.to_owned(), Some(v.to_owned())),
                None => (s.to_owned(), None),
            })
            .collect();
        eprintln!("weld demo: switches {:?}", runtime_config.command_line_switches);
    }
    runtime_config.cache_path = Some(std::env::temp_dir().join("wgpu-weld-demo-cache"));
    let runtime = CefRuntime::initialize(runtime_config)
        .expect("weld: CEF initialize failed");

    let event_loop = EventLoop::new().expect("event loop creation failed");
    event_loop.set_control_flow(ControlFlow::Poll);

    event_loop
        .run_app(&mut DemoApp::new(runtime))
        .expect("event loop error");
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
        // Includes CEF's "hidden cursor" request, which winit expresses as
        // set_cursor_visible rather than an icon; left as Default here.
        S::Custom(_) => I::Default,
        _ => I::Default,
    };
    icon.into()
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
    eprintln!("weld demo: set_scale_factor failed: {err}");
}
