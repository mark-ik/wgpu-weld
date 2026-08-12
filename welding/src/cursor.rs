//! Latest cursor shape reported by CEF.
//!
//! Under windowless rendering the host owns the pointer, so CEF can only ask
//! for a shape and hope someone applies it. Ignore `OnCursorChange` and the
//! page looks inert: no I-beam over text, no hand over links.

use std::sync::Mutex;

use crate::surface::CursorShape;

#[derive(Debug, Default)]
pub(crate) struct LatestCursor(Mutex<Option<CursorShape>>);

impl LatestCursor {
    pub(crate) fn set(&self, shape: CursorShape) {
        *self.0.lock().unwrap() = Some(shape);
    }

    /// Take the pending shape, if it changed since the last poll.
    pub(crate) fn take(&self) -> Option<CursorShape> {
        self.0.lock().unwrap().take()
    }
}

/// Map CEF's cursor type onto the shared vocabulary.
///
/// The two names that matter are crossed, which is exactly the sort of thing
/// that gets miswired once and then looks "nearly right" forever: CEF's
/// `POINTER` is the ordinary arrow, while `CursorShape::Pointer` is the CSS
/// `pointer`, i.e. the link hand, which CEF calls `HAND`.
#[cfg(feature = "cef-runtime")]
pub(crate) fn from_cef(type_: cef::CursorType) -> CursorShape {
    use cef::CursorType as C;

    // Written as comparisons rather than a match: CursorType is a newtype over
    // a #[non_exhaustive] C enum, so its variants are associated consts.
    if type_ == C::POINTER {
        CursorShape::Default
    } else if type_ == C::HAND {
        CursorShape::Pointer
    } else if type_ == C::IBEAM || type_ == C::VERTICALTEXT {
        CursorShape::Text
    } else if type_ == C::CROSS || type_ == C::CELL {
        CursorShape::Crosshair
    } else if type_ == C::WAIT {
        CursorShape::Wait
    } else if type_ == C::PROGRESS {
        CursorShape::Progress
    } else if type_ == C::HELP {
        CursorShape::Help
    } else if type_ == C::MOVE || type_ == C::MIDDLEPANNING {
        CursorShape::Move
    } else if type_ == C::NOTALLOWED || type_ == C::NODROP || type_ == C::DND_NONE {
        CursorShape::NotAllowed
    } else if type_ == C::ZOOMIN {
        CursorShape::ZoomIn
    } else if type_ == C::ZOOMOUT {
        CursorShape::ZoomOut
    } else if type_ == C::GRAB {
        CursorShape::Grab
    } else if type_ == C::GRABBING {
        CursorShape::Grabbing
    } else if type_ == C::NORTHSOUTHRESIZE
        || type_ == C::NORTHRESIZE
        || type_ == C::SOUTHRESIZE
        || type_ == C::ROWRESIZE
    {
        CursorShape::ResizeNs
    } else if type_ == C::EASTWESTRESIZE
        || type_ == C::EASTRESIZE
        || type_ == C::WESTRESIZE
        || type_ == C::COLUMNRESIZE
    {
        CursorShape::ResizeEw
    } else if type_ == C::NORTHEASTSOUTHWESTRESIZE
        || type_ == C::NORTHEASTRESIZE
        || type_ == C::SOUTHWESTRESIZE
    {
        CursorShape::ResizeNeSw
    } else if type_ == C::NORTHWESTSOUTHEASTRESIZE
        || type_ == C::NORTHWESTRESIZE
        || type_ == C::SOUTHEASTRESIZE
    {
        CursorShape::ResizeNwSe
    } else if type_ == C::NONE {
        // The shared vocabulary has no "hidden" shape. Named rather than
        // silently mapped to Default, because a host that wants to honour it
        // can, and one that does not at least sees the difference.
        CursorShape::Custom("none".into())
    } else {
        CursorShape::Custom("cef-other".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latest_cursor_yields_once_per_change() {
        let c = LatestCursor::default();
        assert_eq!(c.take(), None);
        c.set(CursorShape::Text);
        assert_eq!(c.take(), Some(CursorShape::Text));
        assert_eq!(c.take(), None, "a shape should only be reported once");
    }

    #[cfg(feature = "cef-runtime")]
    #[test]
    fn the_crossed_pointer_names_map_the_right_way_round() {
        // CEF POINTER is the arrow; CSS/scrying Pointer is the link hand.
        assert_eq!(from_cef(cef::CursorType::POINTER), CursorShape::Default);
        assert_eq!(from_cef(cef::CursorType::HAND), CursorShape::Pointer);
        assert_eq!(from_cef(cef::CursorType::IBEAM), CursorShape::Text);
    }
}
