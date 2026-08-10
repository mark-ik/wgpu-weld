//! Presenting the imported surfaces, and the unattended verdict.
//!
//! Split out of `main.rs` to stay under the 600-line ceiling.

use crate::{probe, DemoState};

pub(crate) fn render(s: &mut DemoState) {
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

    let make_bg = |view: &wgpu::TextureView| {
        s.host_ctx
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &s.blit.bg_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&s.blit.sampler),
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
            rpass.set_pipeline(&s.blit.pipeline);
            rpass.set_bind_group(0, bg, &[]);
            rpass.draw(0..3, 0..1);
        }

        // The popup goes over the view, clipped to the rect CEF asked for. Same
        // pipeline, different viewport: the fullscreen triangle then covers
        // exactly the popup's rectangle.
        if let (Some(popup), Some(popup_bg)) = (&s.popup, &popup_bg) {
            let vw = s.surface_config.width as f32;
            let vh = s.surface_config.height as f32;
            let x = (popup.rect.x as f32).clamp(0.0, vw);
            let y = (popup.rect.y as f32).clamp(0.0, vh);
            let w = (popup.rect.width as f32).min(vw - x);
            let h = (popup.rect.height as f32).min(vh - y);
            if w > 0.0 && h > 0.0 {
                rpass.set_viewport(x, y, w, h, 0.0, 1.0);
                rpass.set_pipeline(&s.blit.pipeline);
                rpass.set_bind_group(0, popup_bg, &[]);
                rpass.draw(0..3, 0..1);
            }
        }
    }

    s.host_ctx.queue.submit([enc.finish()]);
    output.present();
    s.window.request_redraw();
}

/// Probe the last imported frame and print a verdict a log reader can trust.
pub(crate) fn report(s: &mut DemoState) {
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

    // Popup verdict, only when a popup was actually asked for.
    if s.popups_imported > 0 {
        match s.popup.as_ref() {
            Some(popup) => {
                match probe::sample(&s.host_ctx.device, &s.host_ctx.queue, &popup.texture.texture) {
                    Ok(rb) if rb.looks_painted() => log::info!(
                        "POPUP PASS: {} popup surface(s), {}x{} at {},{}, {}/{} bytes non-zero, \
                         first pixels {:?}",
                        s.popups_imported,
                        popup.rect.width,
                        popup.rect.height,
                        popup.rect.x,
                        popup.rect.y,
                        rb.non_zero_bytes,
                        rb.total_bytes,
                        rb.first_pixels
                    ),
                    Ok(rb) => log::error!(
                        "POPUP FAIL: surface imported but entirely zero ({} bytes)",
                        rb.total_bytes
                    ),
                    Err(e) => log::error!("POPUP FAIL: readback failed: {e}"),
                }
            }
            None => log::error!(
                "POPUP FAIL: {} imported but the cached surface was dropped before the report",
                s.popups_imported
            ),
        }
    }
}

