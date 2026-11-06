//! Keyboard, IME, paste and mouse input translated to what a program in an xterm-compatible
//! terminal expects.
//!
//! - Text and IME commits are sent as typed. While an IME composition is active, keys that
//!   arrive without committing it (arrows, Enter, Escape used to pick candidates) are not sent;
//!   keys that arrive together with the commit are sent after the committed text.
//! - Option acts as Meta (ESC prefix) unless the setting turns that off (see [`key_mods`]);
//!   Ctrl+letter sends control codes; Cmd shortcuts belong to the app (only Cmd+C/V/A/F are
//!   handled here).

use alacritty_terminal::term::TermMode;
use egui::{Key, Modifiers};

/// xterm modifier parameter: 1 + shift + 2·alt + 4·ctrl.
fn mod_param(m: Modifiers) -> u8 {
    1 + m.shift as u8 + 2 * m.alt as u8 + 4 * m.ctrl as u8
}

fn csi_mod(final_byte: char, m: Modifiers) -> String {
    format!("\x1b[1;{}{final_byte}", mod_param(m))
}

/// The modifiers a key press is encoded with. When Option is not Meta, Option+character is left
/// to macOS, which composes a character (Option+e → ´) that arrives as text; named keys
/// (arrows, Backspace, Enter…) keep Option.
pub fn key_mods(key: Key, m: Modifiers, option_as_meta: bool) -> Modifiers {
    if !option_as_meta && m.alt && key_char(key, m.shift).is_some() { Modifiers { alt: false, ..m } } else { m }
}

/// Bytes for a key press, or `None` when the key is not for the terminal.
pub fn key_bytes(key: Key, m: Modifiers, mode: TermMode) -> Option<Vec<u8>> {
    if m.command || m.mac_cmd {
        return None;
    }
    let app_cursor = mode.contains(TermMode::APP_CURSOR);
    let plain = !m.shift && !m.alt && !m.ctrl;
    let arrow = |c: char| {
        if plain { if app_cursor { format!("\x1bO{c}") } else { format!("\x1b[{c}") } } else { csi_mod(c, m) }
    };
    let tilde = |n: u8| if plain { format!("\x1b[{n}~") } else { format!("\x1b[{n};{}~", mod_param(m)) };
    let ss3 = |c: char| if plain { format!("\x1bO{c}") } else { csi_mod(c, m) };
    let s: String = match key {
        Key::Enter => return Some(if m.shift || m.alt { b"\x1b\r".to_vec() } else { b"\r".to_vec() }),
        Key::Tab => return Some(if m.shift { b"\x1b[Z".to_vec() } else { b"\t".to_vec() }),
        Key::Backspace => {
            return Some(match (m.ctrl, m.alt) {
                (true, _) => vec![0x08],
                (false, true) => b"\x1b\x7f".to_vec(),
                _ => vec![0x7f],
            });
        }
        Key::Escape => return Some(vec![0x1b]),
        Key::ArrowUp => arrow('A'),
        Key::ArrowDown => arrow('B'),
        Key::ArrowRight => arrow('C'),
        Key::ArrowLeft => arrow('D'),
        Key::Home => arrow('H'),
        Key::End => arrow('F'),
        Key::PageUp => tilde(5),
        Key::PageDown => tilde(6),
        Key::Insert => tilde(2),
        Key::Delete => tilde(3),
        Key::F1 => ss3('P'),
        Key::F2 => ss3('Q'),
        Key::F3 => ss3('R'),
        Key::F4 => ss3('S'),
        Key::F5 => tilde(15),
        Key::F6 => tilde(17),
        Key::F7 => tilde(18),
        Key::F8 => tilde(19),
        Key::F9 => tilde(20),
        Key::F10 => tilde(21),
        Key::F11 => tilde(23),
        Key::F12 => tilde(24),
        Key::Space if m.ctrl => return Some(with_meta(vec![0], m.alt)),
        _ => {
            let ch = key_char(key, m.shift)?;
            if m.ctrl {
                return control(ch).map(|b| with_meta(vec![b], m.alt));
            }
            if m.alt {
                // Option as Meta.
                let mut v = vec![0x1b];
                let mut buf = [0u8; 4];
                v.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                return Some(v);
            }
            return None; // plain characters arrive as text events
        }
    };
    Some(s.into_bytes())
}

fn with_meta(mut v: Vec<u8>, alt: bool) -> Vec<u8> {
    if alt {
        v.insert(0, 0x1b);
    }
    v
}

/// Ctrl+key control codes.
fn control(ch: char) -> Option<u8> {
    match ch {
        'a'..='z' => Some(ch as u8 - b'a' + 1),
        'A'..='Z' => Some(ch as u8 - b'A' + 1),
        '@' | '2' | ' ' => Some(0),
        '[' | '3' => Some(0x1b),
        '\\' | '4' => Some(0x1c),
        ']' | '5' => Some(0x1d),
        '^' | '6' => Some(0x1e),
        '_' | '-' | '/' | '7' => Some(0x1f),
        '?' | '8' => Some(0x7f),
        _ => None,
    }
}

/// Character of a key on a US layout (used for Ctrl and Option combinations, which do not
/// produce text events).
fn key_char(key: Key, shift: bool) -> Option<char> {
    let name = key.name();
    if name.len() == 1 {
        let c = name.chars().next()?;
        if c.is_ascii_alphabetic() {
            return Some(if shift { c.to_ascii_uppercase() } else { c.to_ascii_lowercase() });
        }
    }
    let (plain, shifted) = match key {
        Key::Num0 => ('0', ')'),
        Key::Num1 => ('1', '!'),
        Key::Num2 => ('2', '@'),
        Key::Num3 => ('3', '#'),
        Key::Num4 => ('4', '$'),
        Key::Num5 => ('5', '%'),
        Key::Num6 => ('6', '^'),
        Key::Num7 => ('7', '&'),
        Key::Num8 => ('8', '*'),
        Key::Num9 => ('9', '('),
        Key::Minus => ('-', '_'),
        Key::Equals => ('=', '+'),
        Key::OpenBracket => ('[', '{'),
        Key::CloseBracket => (']', '}'),
        Key::Backslash => ('\\', '|'),
        Key::Semicolon => (';', ':'),
        Key::Quote => ('\'', '"'),
        Key::Comma => (',', '<'),
        Key::Period => ('.', '>'),
        Key::Slash => ('/', '?'),
        Key::Backtick => ('`', '~'),
        Key::Space => (' ', ' '),
        _ => return None,
    };
    Some(if shift { shifted } else { plain })
}

/// Whether pasting `text` must be confirmed first: without bracketed paste the program gets
/// each line break as Enter, so every line would run as it arrives.
pub fn paste_needs_confirm(text: &str, mode: TermMode) -> bool {
    !mode.contains(TermMode::BRACKETED_PASTE) && text.contains(['\n', '\r'])
}

/// Pasted text as the program should receive it.
pub fn paste_bytes(text: &str, mode: TermMode) -> Vec<u8> {
    // ESC (and the 8-bit CSI) could end bracketed paste early or inject sequences.
    let clean: String = text.chars().filter(|c| *c != '\x1b' && *c != '\u{9b}').collect();
    if mode.contains(TermMode::BRACKETED_PASTE) {
        format!("\x1b[200~{clean}\x1b[201~").into_bytes()
    } else {
        clean.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
    }
}

/// Mouse button as encoded in reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Left = 0,
    Middle = 1,
    Right = 2,
    WheelUp = 64,
    WheelDown = 65,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseAction {
    Press,
    Release,
    /// Movement with `Some(button)` held, or without a button (`None`).
    Motion(Option<Button>),
}

/// A mouse report for a cell (`col`, `row` from 0), or `None` when the mode does not want it.
pub fn mouse_report(
    action: MouseAction,
    button: Button,
    col: usize,
    row: usize,
    m: Modifiers,
    mode: TermMode,
) -> Option<Vec<u8>> {
    let wants = match action {
        MouseAction::Press | MouseAction::Release => mode.intersects(TermMode::MOUSE_MODE),
        MouseAction::Motion(Some(_)) => mode.intersects(TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION),
        MouseAction::Motion(None) => mode.contains(TermMode::MOUSE_MOTION),
    };
    if !wants {
        return None;
    }
    let mut code = match action {
        MouseAction::Motion(b) => 32 + b.map_or(3, |b| b as u8),
        _ => button as u8,
    };
    code += 4 * m.shift as u8 + 8 * m.alt as u8 + 16 * m.ctrl as u8;
    if mode.contains(TermMode::SGR_MOUSE) {
        let end = if action == MouseAction::Release { 'm' } else { 'M' };
        return Some(format!("\x1b[<{code};{};{}{end}", col + 1, row + 1).into_bytes());
    }
    // Legacy encoding: release has no button; coordinates are capped.
    if action == MouseAction::Release {
        code = 3 + (code & !3);
    }
    let enc = |v: usize| (32 + (v + 1).min(223)) as u8;
    Some(vec![0x1b, b'[', b'M', 32 + code, enc(col), enc(row)])
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: Modifiers = Modifiers::NONE;

    #[test]
    fn option_is_meta_only_when_set() {
        let alt = Modifiers { alt: true, ..NONE };
        let ctrl_alt = Modifiers { alt: true, ctrl: true, ..NONE };
        let plain = TermMode::empty();
        // As Meta: ESC prefix, the composed character is dropped by the caller.
        assert_eq!(key_bytes(Key::E, key_mods(Key::E, alt, true), plain), Some(b"\x1be".to_vec()));
        // Not Meta: nothing for the key itself; macOS sends the composed text instead.
        assert_eq!(key_mods(Key::E, alt, false), NONE);
        assert_eq!(key_bytes(Key::E, key_mods(Key::E, alt, false), plain), None);
        assert_eq!(key_bytes(Key::C, key_mods(Key::C, ctrl_alt, false), plain), Some(vec![0x03]));
        // Named keys keep Option either way (word movement, word delete).
        assert_eq!(key_mods(Key::ArrowLeft, alt, false), alt);
        assert_eq!(key_bytes(Key::Backspace, key_mods(Key::Backspace, alt, false), plain), Some(b"\x1b\x7f".to_vec()));
    }

    fn m(shift: bool, alt: bool, ctrl: bool) -> Modifiers {
        Modifiers { shift, alt, ctrl, ..Default::default() }
    }

    #[test]
    fn named_keys_follow_xterm() {
        let normal = TermMode::default();
        assert_eq!(key_bytes(Key::Enter, NONE, normal).unwrap(), b"\r");
        assert_eq!(key_bytes(Key::Enter, m(true, false, false), normal).unwrap(), b"\x1b\r");
        assert_eq!(key_bytes(Key::Tab, m(true, false, false), normal).unwrap(), b"\x1b[Z");
        assert_eq!(key_bytes(Key::Backspace, NONE, normal).unwrap(), b"\x7f");
        assert_eq!(key_bytes(Key::ArrowUp, NONE, normal).unwrap(), b"\x1b[A");
        assert_eq!(key_bytes(Key::ArrowUp, NONE, TermMode::APP_CURSOR).unwrap(), b"\x1bOA");
        assert_eq!(key_bytes(Key::ArrowLeft, m(false, true, false), normal).unwrap(), b"\x1b[1;3D");
        assert_eq!(key_bytes(Key::ArrowRight, m(true, false, true), normal).unwrap(), b"\x1b[1;6C");
        assert_eq!(key_bytes(Key::Delete, NONE, normal).unwrap(), b"\x1b[3~");
        assert_eq!(key_bytes(Key::PageUp, m(true, false, false), normal).unwrap(), b"\x1b[5;2~");
        assert_eq!(key_bytes(Key::F1, NONE, normal).unwrap(), b"\x1bOP");
        assert_eq!(key_bytes(Key::F5, NONE, normal).unwrap(), b"\x1b[15~");
    }

    #[test]
    fn control_meta_and_command() {
        let normal = TermMode::default();
        assert_eq!(key_bytes(Key::C, m(false, false, true), normal).unwrap(), [3]);
        assert_eq!(key_bytes(Key::OpenBracket, m(false, false, true), normal).unwrap(), [0x1b]);
        assert_eq!(key_bytes(Key::Space, m(false, false, true), normal).unwrap(), [0]);
        assert_eq!(key_bytes(Key::B, m(false, true, false), normal).unwrap(), b"\x1bb");
        assert_eq!(key_bytes(Key::Period, m(true, true, false), normal).unwrap(), b"\x1b>");
        assert_eq!(key_bytes(Key::A, NONE, normal), None, "plain text comes from text events");
        let cmd = Modifiers { command: true, mac_cmd: true, ..Default::default() };
        assert_eq!(key_bytes(Key::K, cmd, normal), None, "app shortcuts are not sent");
    }

    #[test]
    fn paste_strips_escapes_and_brackets_when_asked() {
        assert_eq!(paste_bytes("a\nb\x1b[31m", TermMode::default()), b"a\rb[31m");
        assert_eq!(paste_bytes("한\r\n글", TermMode::BRACKETED_PASTE), "\x1b[200~한\r\n글\x1b[201~".as_bytes());
    }

    #[test]
    fn line_breaks_without_bracketed_paste_need_confirmation() {
        let plain = TermMode::default();
        assert!(paste_needs_confirm("ls\nrm -rf build", plain));
        assert!(paste_needs_confirm("make deploy\r", plain), "one line that would press Enter");
        assert!(!paste_needs_confirm("one line", plain));
        assert!(!paste_needs_confirm("a\nb", TermMode::BRACKETED_PASTE), "the program receives it as one paste");
    }

    #[test]
    fn mouse_reports_in_sgr_and_legacy_encodings() {
        let sgr = TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE;
        assert_eq!(mouse_report(MouseAction::Press, Button::Left, 4, 2, NONE, sgr).unwrap(), b"\x1b[<0;5;3M");
        assert_eq!(mouse_report(MouseAction::Release, Button::Left, 4, 2, NONE, sgr).unwrap(), b"\x1b[<0;5;3m");
        assert_eq!(mouse_report(MouseAction::Press, Button::WheelUp, 0, 0, NONE, sgr).unwrap(), b"\x1b[<64;1;1M");
        assert_eq!(mouse_report(MouseAction::Motion(Some(Button::Left)), Button::Left, 0, 0, NONE, sgr), None);
        let drag = TermMode::MOUSE_DRAG | TermMode::SGR_MOUSE;
        assert_eq!(
            mouse_report(MouseAction::Motion(Some(Button::Left)), Button::Left, 1, 1, NONE, drag).unwrap(),
            b"\x1b[<32;2;2M"
        );
        let legacy = TermMode::MOUSE_REPORT_CLICK;
        assert_eq!(
            mouse_report(MouseAction::Press, Button::Left, 0, 0, NONE, legacy).unwrap(),
            [0x1b, b'[', b'M', 32, 33, 33]
        );
        assert_eq!(
            mouse_report(MouseAction::Release, Button::Left, 0, 0, NONE, legacy).unwrap(),
            [0x1b, b'[', b'M', 35, 33, 33]
        );
        assert_eq!(mouse_report(MouseAction::Press, Button::Left, 0, 0, NONE, TermMode::default()), None);
    }
}
