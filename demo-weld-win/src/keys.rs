//! CEF uses Windows virtual-key codes on every platform.

use winit::keyboard::KeyCode;

#[rustfmt::skip]
pub(crate) fn keycode_to_vk(kc: KeyCode) -> i32 {
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
