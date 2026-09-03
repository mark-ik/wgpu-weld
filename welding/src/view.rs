// Copyright 2026 Mark Alan Boykin
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0

//! View geometry: physical size plus the display scale factor.
//!
//! CEF's windowless model separates the two, and getting it wrong is quiet
//! rather than loud:
//!
//! - `GetViewRect` is answered in **DIP** (device-independent pixels, i.e. CSS
//!   pixels), not physical pixels.
//! - `GetScreenInfo::device_scale_factor` is the multiplier CEF applies to
//!   produce the physical texture it paints.
//! - Mouse coordinates handed to CEF are in DIP as well.
//!
//! Answer `GetViewRect` in physical pixels with a scale of 1, as welding did
//! before, and a 2x display gets a page laid out at twice the CSS width, so
//! everything renders at half its intended size. Feed it physical mouse
//! coordinates under a real scale and clicks land at the wrong place. Both
//! failure modes look like a working browser, which is why this lives in one
//! place with tests rather than being open-coded per platform.
//!
//! Hosts talk to welding in **physical** pixels throughout, because that is
//! what a window system hands them; the DIP conversion happens here.

use dpi::PhysicalSize;

use crate::surface::PopupRect;

#[derive(Debug, Clone, Copy)]
pub(crate) struct ViewMetrics {
    size: PhysicalSize<u32>,
    scale: f32,
}

impl ViewMetrics {
    pub(crate) fn new(size: PhysicalSize<u32>, scale: f32) -> Self {
        Self {
            size,
            scale: sane_scale(scale),
        }
    }

    pub(crate) fn set_size(&mut self, size: PhysicalSize<u32>) {
        self.size = size;
    }

    pub(crate) fn set_scale(&mut self, scale: f32) {
        self.scale = sane_scale(scale);
    }

    pub(crate) fn scale(&self) -> f32 {
        self.scale
    }

    /// What `GetViewRect` should report: the view in DIP.
    pub(crate) fn logical(&self) -> (i32, i32) {
        let w = (self.size.width as f32 / self.scale).round().max(1.0);
        let h = (self.size.height as f32 / self.scale).round().max(1.0);
        (w as i32, h as i32)
    }

    /// Host physical coordinates to the DIP that CEF expects for input.
    pub(crate) fn point_to_dip(&self, x: i32, y: i32) -> (i32, i32) {
        (
            (x as f32 / self.scale).round() as i32,
            (y as f32 / self.scale).round() as i32,
        )
    }

    /// CEF hands popup geometry back in DIP; hosts draw in physical pixels.
    pub(crate) fn rect_to_physical(&self, rect: PopupRect) -> PopupRect {
        PopupRect {
            x: (rect.x as f32 * self.scale).round() as i32,
            y: (rect.y as f32 * self.scale).round() as i32,
            width: (rect.width as f32 * self.scale).round() as u32,
            height: (rect.height as f32 * self.scale).round() as u32,
        }
    }
}

/// A zero or negative scale would divide the view to nothing, and CEF rejects
/// absurd factors anyway. Clamp rather than panic: a bad scale from a host is
/// not worth taking the browser down for.
fn sane_scale(scale: f32) -> f32 {
    if scale.is_finite() {
        scale.clamp(0.25, 8.0)
    } else {
        1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unscaled_view_is_reported_as_is() {
        let m = ViewMetrics::new(PhysicalSize::new(1280, 800), 1.0);
        assert_eq!(m.logical(), (1280, 800));
        assert_eq!(m.point_to_dip(640, 400), (640, 400));
    }

    #[test]
    fn hidpi_view_is_reported_in_dip() {
        let m = ViewMetrics::new(PhysicalSize::new(2560, 1600), 2.0);
        // CEF lays out 1280x800 CSS pixels and paints a 2560x1600 texture.
        assert_eq!(m.logical(), (1280, 800));
        // A click at the physical centre is the DIP centre.
        assert_eq!(m.point_to_dip(1280, 800), (640, 400));
        // A popup CEF places at DIP 40,80 is drawn at physical 80,160.
        assert_eq!(
            m.rect_to_physical(PopupRect {
                x: 40,
                y: 80,
                width: 200,
                height: 95
            }),
            PopupRect {
                x: 80,
                y: 160,
                width: 400,
                height: 190
            }
        );
    }

    #[test]
    fn fractional_scale_rounds_without_collapsing() {
        let m = ViewMetrics::new(PhysicalSize::new(1920, 1080), 1.5);
        assert_eq!(m.logical(), (1280, 720));
    }

    #[test]
    fn nonsense_scales_are_clamped_not_fatal() {
        assert_eq!(
            ViewMetrics::new(PhysicalSize::new(800, 600), 0.0).scale(),
            0.25
        );
        assert_eq!(
            ViewMetrics::new(PhysicalSize::new(800, 600), -3.0).scale(),
            0.25
        );
        assert_eq!(
            ViewMetrics::new(PhysicalSize::new(800, 600), f32::NAN).scale(),
            1.0
        );
        // A degenerate view still reports at least one DIP.
        let m = ViewMetrics::new(PhysicalSize::new(1, 1), 8.0);
        assert_eq!(m.logical(), (1, 1));
    }
}
