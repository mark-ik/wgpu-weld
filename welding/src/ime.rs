// Copyright 2026 Mark Alan Boykin
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0

//! IME composition feedback from CEF.
//!
//! The host owns the input method under windowless rendering, so it needs to
//! know where the text being composed actually sits on screen; that is where
//! the candidate window goes. CEF reports it through
//! `OnImeCompositionRangeChanged`, in DIP.

use std::sync::Mutex;

use crate::surface::{ImeComposition, PopupRect};

#[derive(Debug, Default)]
pub(crate) struct LatestComposition(Mutex<Option<ImeComposition>>);

impl LatestComposition {
    pub(crate) fn set(&self, composition: ImeComposition) {
        *self.0.lock().unwrap() = Some(composition);
    }

    pub(crate) fn take(&self) -> Option<ImeComposition> {
        self.0.lock().unwrap().take()
    }
}

/// The union of the character bounds CEF reports, which is the rectangle a
/// host wants to sit its candidate window under. CEF hands over one rect per
/// character; a host almost never wants them individually.
pub(crate) fn bounds_union(rects: &[PopupRect]) -> Option<PopupRect> {
    let mut it = rects.iter().filter(|r| r.width > 0 && r.height > 0);
    let first = *it.next()?;
    let (mut x0, mut y0) = (first.x, first.y);
    let (mut x1, mut y1) = (first.x + first.width as i32, first.y + first.height as i32);
    for r in it {
        x0 = x0.min(r.x);
        y0 = y0.min(r.y);
        x1 = x1.max(r.x + r.width as i32);
        y1 = y1.max(r.y + r.height as i32);
    }
    Some(PopupRect {
        x: x0,
        y: y0,
        width: (x1 - x0).max(0) as u32,
        height: (y1 - y0).max(0) as u32,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(x: i32, y: i32, w: u32, h: u32) -> PopupRect {
        PopupRect {
            x,
            y,
            width: w,
            height: h,
        }
    }

    #[test]
    fn union_spans_every_character() {
        let got = bounds_union(&[r(10, 20, 8, 16), r(18, 20, 8, 16), r(26, 20, 8, 16)]);
        assert_eq!(got, Some(r(10, 20, 24, 16)));
    }

    #[test]
    fn empty_rects_are_ignored_not_counted() {
        // CEF pads with zero-size rects; letting one in would drag the union
        // to the origin and put the candidate window in the corner.
        assert_eq!(
            bounds_union(&[r(0, 0, 0, 0), r(40, 60, 10, 12)]),
            Some(r(40, 60, 10, 12))
        );
        assert_eq!(bounds_union(&[]), None);
        assert_eq!(bounds_union(&[r(0, 0, 0, 0)]), None);
    }
}
