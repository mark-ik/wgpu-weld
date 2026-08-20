//! Shared popup-widget state.
//!
//! CEF reports popup geometry and visibility through two render-handler
//! callbacks (`OnPopupSize`, `OnPopupShow`) that arrive independently of the
//! paint callback, and on the CEF UI thread rather than the host's. All three
//! platform producers need the same small piece of shared state, so it lives
//! here rather than three times over.
//!
//! The per-platform *frame* slot stays per-platform: Windows imports the popup
//! texture inside the paint callback, while macOS and Linux carry a native
//! frame across to `acquire_popup`.

use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
};

use crate::surface::PopupRect;

#[derive(Debug, Default)]
pub(crate) struct PopupState {
    visible: AtomicBool,
    rect: Mutex<PopupRect>,
}

impl PopupState {
    /// `OnPopupShow`. Hiding is the interesting direction: CEF hides a popup
    /// without painting anything, so a host waiting on frames alone would
    /// leave a stale dropdown on screen.
    pub(crate) fn set_visible(&self, visible: bool) {
        self.visible.store(visible, Ordering::Release);
    }

    /// `OnPopupSize`. Arrives before the first popup paint.
    pub(crate) fn set_rect(&self, rect: PopupRect) {
        *self.rect.lock().unwrap() = rect;
    }

    pub(crate) fn is_visible(&self) -> bool {
        self.visible.load(Ordering::Acquire)
    }

    /// The placement a host should draw at, or `None` when nothing is open.
    pub(crate) fn rect_if_visible(&self) -> Option<PopupRect> {
        if !self.is_visible() {
            return None;
        }
        let rect = *self.rect.lock().unwrap();
        // CEF has been observed to show a popup before sizing it; a zero-area
        // rect is not something a host can draw.
        (rect.width > 0 && rect.height > 0).then_some(rect)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_popup_has_no_placement() {
        let state = PopupState::default();
        state.set_rect(PopupRect {
            x: 10,
            y: 20,
            width: 100,
            height: 50,
        });
        assert_eq!(state.rect_if_visible(), None);

        state.set_visible(true);
        assert_eq!(
            state.rect_if_visible(),
            Some(PopupRect {
                x: 10,
                y: 20,
                width: 100,
                height: 50
            })
        );

        state.set_visible(false);
        assert_eq!(state.rect_if_visible(), None);
    }

    #[test]
    fn visible_but_unsized_popup_has_no_placement() {
        let state = PopupState::default();
        state.set_visible(true);
        assert_eq!(state.rect_if_visible(), None);
    }
}
