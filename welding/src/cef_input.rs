/// CEF input translation: weld surface types → cef BrowserHost send methods.
///
/// Only compiled under `cef-runtime`. All functions take `&cef::BrowserHost`
/// (the cef crate's methods are `&self`, not `&mut self`).
use cef::ImplBrowserHost;
use crate::surface::{EventModifiers, FocusDirection, KeyEvent, KeyEventKind, MouseAction,
    MouseButton, MouseEvent};

// CEF event-flag bitmasks — stable ABI values from cef_event_flags_t.
const EVENTFLAG_SHIFT_DOWN:   u32 = 1 << 1;
const EVENTFLAG_CONTROL_DOWN: u32 = 1 << 2;
const EVENTFLAG_ALT_DOWN:     u32 = 1 << 3;
const EVENTFLAG_LEFT_MOUSE_BUTTON: u32 = 1 << 4;
const EVENTFLAG_MIDDLE_MOUSE_BUTTON: u32 = 1 << 5;
const EVENTFLAG_RIGHT_MOUSE_BUTTON: u32 = 1 << 6;
const EVENTFLAG_COMMAND_DOWN: u32 = 1 << 7; // macOS Cmd / Windows Meta

fn modifiers(m: EventModifiers) -> u32 {
    let mut f = 0u32;
    if m.shift { f |= EVENTFLAG_SHIFT_DOWN; }
    if m.ctrl  { f |= EVENTFLAG_CONTROL_DOWN; }
    if m.alt   { f |= EVENTFLAG_ALT_DOWN; }
    if m.left_mouse_button { f |= EVENTFLAG_LEFT_MOUSE_BUTTON; }
    if m.middle_mouse_button { f |= EVENTFLAG_MIDDLE_MOUSE_BUTTON; }
    if m.right_mouse_button { f |= EVENTFLAG_RIGHT_MOUSE_BUTTON; }
    if m.meta  { f |= EVENTFLAG_COMMAND_DOWN; }
    f
}

pub fn send_mouse(host: &cef::BrowserHost, ev: &MouseEvent) {
    let cef_ev = cef::MouseEvent { x: ev.x, y: ev.y, modifiers: modifiers(ev.modifiers) };
    match ev.action {
        MouseAction::Pressed => {
            host.send_mouse_click_event(Some(&cef_ev), cef_button(ev.button), 0, 1);
        }
        MouseAction::Released => {
            host.send_mouse_click_event(Some(&cef_ev), cef_button(ev.button), 1, 1);
        }
        MouseAction::Moved => {
            host.send_mouse_move_event(Some(&cef_ev), 0);
        }
        MouseAction::WheelScrolled { delta_x, delta_y } => {
            host.send_mouse_wheel_event(Some(&cef_ev), delta_x, delta_y);
        }
    }
}

fn cef_button(b: MouseButton) -> cef::MouseButtonType {
    match b {
        MouseButton::Left   => cef::MouseButtonType::LEFT,
        MouseButton::Middle => cef::MouseButtonType::MIDDLE,
        MouseButton::Right  => cef::MouseButtonType::RIGHT,
    }
}

pub fn send_key(host: &cef::BrowserHost, ev: &KeyEvent) {
    let type_ = match ev.kind {
        KeyEventKind::RawKeyDown => cef::KeyEventType::RAWKEYDOWN,
        KeyEventKind::KeyDown    => cef::KeyEventType::KEYDOWN,
        KeyEventKind::KeyUp      => cef::KeyEventType::KEYUP,
        KeyEventKind::Char       => cef::KeyEventType::CHAR,
    };
    let ch = ev.character.map(|c| c as u16).unwrap_or(0);
    let cef_ev = cef::KeyEvent {
        type_,
        modifiers: modifiers(ev.modifiers),
        windows_key_code: ev.windows_key_code,
        native_key_code: ev.native_key_code,
        character: ch,
        unmodified_character: ch,
        ..cef::KeyEvent::default()
    };
    host.send_key_event(Some(&cef_ev));
}

/// Give input focus to the browser. Direction is unused by CEF's `set_focus`;
/// use `send_keyboard_input` with Tab/Shift-Tab for in-page focus cycling.
pub fn set_focus(host: &cef::BrowserHost, _direction: FocusDirection) {
    host.set_focus(1);
}
