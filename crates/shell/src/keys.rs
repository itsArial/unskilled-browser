//! winit keyboard events → the Windows virtual-key codes and XKB keycodes Chromium expects.

use engine::{Key, KeyKind, modifiers};
use winit::{
    event::{ElementState, KeyEvent},
    keyboard::{KeyCode, ModifiersState, PhysicalKey},
    platform::scancode::PhysicalKeyExtScancode,
};

pub fn flags(m: ModifiersState) -> u32 {
    let mut f = 0;
    if m.shift_key() {
        f |= modifiers::SHIFT;
    }
    if m.control_key() {
        f |= modifiers::CONTROL;
    }
    if m.alt_key() {
        f |= modifiers::ALT;
    }
    if m.super_key() {
        f |= modifiers::COMMAND;
    }
    f
}

/// Key down/up plus any CHAR events the page should receive.
pub fn translate(event: &KeyEvent, mods: ModifiersState) -> Vec<Key> {
    let modifiers = flags(mods);
    let windows_code = match event.physical_key {
        PhysicalKey::Code(code) => windows_code(code),
        PhysicalKey::Unidentified(_) => 0,
    };
    let native_code = native_code(event);
    let base = Key { kind: KeyKind::Down, windows_code, native_code, character: 0, modifiers };

    let mut keys = Vec::new();
    if event.state == ElementState::Released {
        keys.push(Key { kind: KeyKind::Up, ..base });
        return keys;
    }
    keys.push(base);
    // Text input: no CHAR for shortcuts or control characters (except Enter).
    if mods.control_key() || mods.alt_key() || mods.super_key() {
        return keys;
    }
    if let Some(text) = &event.text {
        for unit in text.encode_utf16() {
            if unit >= 0x20 || unit == 0x0D {
                keys.push(Key { kind: KeyKind::Char, character: unit, ..base });
            }
        }
    }
    keys
}

fn windows_code(code: KeyCode) -> i32 {
    use KeyCode::*;
    match code {
        KeyA => 0x41, KeyB => 0x42, KeyC => 0x43, KeyD => 0x44, KeyE => 0x45, KeyF => 0x46,
        KeyG => 0x47, KeyH => 0x48, KeyI => 0x49, KeyJ => 0x4A, KeyK => 0x4B, KeyL => 0x4C,
        KeyM => 0x4D, KeyN => 0x4E, KeyO => 0x4F, KeyP => 0x50, KeyQ => 0x51, KeyR => 0x52,
        KeyS => 0x53, KeyT => 0x54, KeyU => 0x55, KeyV => 0x56, KeyW => 0x57, KeyX => 0x58,
        KeyY => 0x59, KeyZ => 0x5A,
        Digit0 => 0x30, Digit1 => 0x31, Digit2 => 0x32, Digit3 => 0x33, Digit4 => 0x34,
        Digit5 => 0x35, Digit6 => 0x36, Digit7 => 0x37, Digit8 => 0x38, Digit9 => 0x39,
        F1 => 0x70, F2 => 0x71, F3 => 0x72, F4 => 0x73, F5 => 0x74, F6 => 0x75,
        F7 => 0x76, F8 => 0x77, F9 => 0x78, F10 => 0x79, F11 => 0x7A, F12 => 0x7B,
        Enter | NumpadEnter => 0x0D,
        Escape => 0x1B,
        Backspace => 0x08,
        Tab => 0x09,
        Space => 0x20,
        Delete => 0x2E,
        Insert => 0x2D,
        Home => 0x24,
        End => 0x23,
        PageUp => 0x21,
        PageDown => 0x22,
        ArrowLeft => 0x25,
        ArrowUp => 0x26,
        ArrowRight => 0x27,
        ArrowDown => 0x28,
        ShiftLeft | ShiftRight => 0x10,
        ControlLeft | ControlRight => 0x11,
        AltLeft | AltRight => 0x12,
        SuperLeft | SuperRight => 0x5B,
        CapsLock => 0x14,
        Semicolon => 0xBA,
        Equal => 0xBB,
        Comma => 0xBC,
        Minus => 0xBD,
        Period => 0xBE,
        Slash => 0xBF,
        Backquote => 0xC0,
        BracketLeft => 0xDB,
        Backslash => 0xDC,
        BracketRight => 0xDD,
        Quote => 0xDE,
        Numpad0 => 0x60, Numpad1 => 0x61, Numpad2 => 0x62, Numpad3 => 0x63, Numpad4 => 0x64,
        Numpad5 => 0x65, Numpad6 => 0x66, Numpad7 => 0x67, Numpad8 => 0x68, Numpad9 => 0x69,
        NumpadMultiply => 0x6A,
        NumpadAdd => 0x6B,
        NumpadSubtract => 0x6D,
        NumpadDecimal => 0x6E,
        NumpadDivide => 0x6F,
        _ => 0,
    }
}

/// The key's platform code as CEF expects it.
#[cfg(not(windows))]
fn native_code(event: &KeyEvent) -> i32 {
    // XKB keycodes are evdev codes + 8.
    event.physical_key.to_scancode().map_or(0, |s| s as i32 + 8)
}

/// Windows: the `lParam` of the key message (scan code in bits 16–23, extended flag in bit 24).
#[cfg(windows)]
fn native_code(event: &KeyEvent) -> i32 {
    event.physical_key.to_scancode().map_or(0, |s| {
        let extended = if s & 0xE000 == 0xE000 { 1 << 24 } else { 0 };
        (((s & 0xFF) << 16) | extended | 1) as i32
    })
}
