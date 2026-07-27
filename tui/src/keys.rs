//! Translates a crossterm key event into the bytes a terminal would send
//! down the pty for that key, so the wrapped harness receives real input
//! rather than a rendered transcript.
//!
//! This is the pure half of the input path: `child.rs` owns writing the
//! bytes, `main.rs` owns which keys are claimed by the TUI, and this file
//! owns nothing but the encoding. Keeping it free of I/O is what makes the
//! escape sequences testable.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

/// The child's own input modes, read back from the terminal emulator.
/// DECCKM (application cursor keys) changes what the arrow and Home/End
/// keys must send, and a full-screen harness turns it on, so encoding
/// without it sends the wrong sequence exactly when it matters most.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Modes {
    pub application_cursor: bool,
}

impl Modes {
    pub fn application_cursor(application_cursor: bool) -> Self {
        Modes { application_cursor }
    }
}

/// Encodes `key` as pty input bytes, or `None` when the key has no
/// meaningful byte sequence and should simply be dropped.
pub fn encode(key: KeyEvent, modes: Modes) -> Option<Vec<u8>> {
    // Windows and some terminals report press *and* release for every key;
    // forwarding both would double every keystroke.
    if key.kind == KeyEventKind::Release {
        return None;
    }

    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);

    let bytes = match key.code {
        KeyCode::Char(c) => {
            if ctrl {
                vec![control_byte(c)?]
            } else {
                c.to_string().into_bytes()
            }
        }
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Backspace => vec![if ctrl { 0x08 } else { 0x7f }],
        KeyCode::Esc => vec![0x1b],
        KeyCode::Null => vec![0x00],
        KeyCode::Up => cursor_key(b'A', key.modifiers, modes),
        KeyCode::Down => cursor_key(b'B', key.modifiers, modes),
        KeyCode::Right => cursor_key(b'C', key.modifiers, modes),
        KeyCode::Left => cursor_key(b'D', key.modifiers, modes),
        KeyCode::Home => cursor_key(b'H', key.modifiers, modes),
        KeyCode::End => cursor_key(b'F', key.modifiers, modes),
        KeyCode::Insert => tilde_key(2, key.modifiers),
        KeyCode::Delete => tilde_key(3, key.modifiers),
        KeyCode::PageUp => tilde_key(5, key.modifiers),
        KeyCode::PageDown => tilde_key(6, key.modifiers),
        KeyCode::F(n) => function_key(n, key.modifiers)?,
        _ => return None,
    };

    // Alt is transmitted as an ESC prefix, except where the sequence is
    // already an escape sequence carrying its own modifier parameter.
    if alt && !bytes.starts_with(&[0x1b]) {
        let mut prefixed = vec![0x1b];
        prefixed.extend(bytes);
        return Some(prefixed);
    }
    Some(bytes)
}

/// The C0 control byte a `Ctrl+<char>` chord sends, following the usual
/// xterm mapping. `None` for chords with no control byte, so they are
/// dropped rather than sent as their bare character.
fn control_byte(c: char) -> Option<u8> {
    match c.to_ascii_lowercase() {
        c @ 'a'..='z' => Some(c as u8 - b'a' + 1),
        ' ' | '@' | '2' => Some(0x00),
        '[' | '3' => Some(0x1b),
        '\\' | '4' => Some(0x1c),
        ']' | '5' => Some(0x1d),
        '^' | '6' => Some(0x1e),
        '_' | '7' | '/' => Some(0x1f),
        '?' | '8' => Some(0x7f),
        _ => None,
    }
}

/// Arrow and Home/End keys: `SS3` form under application cursor mode,
/// `CSI` form otherwise, and always the parameterized `CSI` form when a
/// modifier is held, since `SS3` has nowhere to carry one.
fn cursor_key(final_byte: u8, modifiers: KeyModifiers, modes: Modes) -> Vec<u8> {
    match modifier_param(modifiers) {
        Some(param) => format!("\x1b[1;{param}{}", final_byte as char).into_bytes(),
        None if modes.application_cursor => vec![0x1b, b'O', final_byte],
        None => vec![0x1b, b'[', final_byte],
    }
}

/// Keys in the `CSI <n> ~` family (Insert, Delete, PageUp, PageDown, and
/// the higher function keys).
fn tilde_key(number: u8, modifiers: KeyModifiers) -> Vec<u8> {
    match modifier_param(modifiers) {
        Some(param) => format!("\x1b[{number};{param}~").into_bytes(),
        None => format!("\x1b[{number}~").into_bytes(),
    }
}

fn function_key(n: u8, modifiers: KeyModifiers) -> Option<Vec<u8>> {
    match n {
        1..=4 => {
            let final_byte = b'P' + (n - 1);
            Some(match modifier_param(modifiers) {
                Some(param) => format!("\x1b[1;{param}{}", final_byte as char).into_bytes(),
                None => vec![0x1b, b'O', final_byte],
            })
        }
        5 => Some(tilde_key(15, modifiers)),
        6..=10 => Some(tilde_key(11 + n, modifiers)),
        11..=12 => Some(tilde_key(12 + n, modifiers)),
        _ => None,
    }
}

/// The xterm modifier parameter: 1 plus a bitmask of shift, alt, control.
/// `None` when no modifier is held, so callers can emit the shorter
/// unparameterized sequence.
fn modifier_param(modifiers: KeyModifiers) -> Option<u8> {
    let mut mask = 0;
    if modifiers.contains(KeyModifiers::SHIFT) {
        mask |= 1;
    }
    if modifiers.contains(KeyModifiers::ALT) {
        mask |= 2;
    }
    if modifiers.contains(KeyModifiers::CONTROL) {
        mask |= 4;
    }
    (mask != 0).then_some(mask + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn chord(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    fn encoded(key: KeyEvent) -> Vec<u8> {
        encode(key, Modes::default()).expect("key should encode to input bytes")
    }

    /// The captain's blocker: Ctrl+C did nothing. It must reach the child
    /// as an interrupt, not be swallowed by the TUI.
    #[test]
    fn ctrl_c_encodes_to_the_interrupt_byte() {
        let bytes = encoded(chord(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert_eq!(bytes, vec![0x03]);
    }

    #[test]
    fn ctrl_letters_map_across_the_whole_c0_range() {
        assert_eq!(encoded(chord(KeyCode::Char('a'), KeyModifiers::CONTROL)), vec![0x01]);
        assert_eq!(encoded(chord(KeyCode::Char('d'), KeyModifiers::CONTROL)), vec![0x04]);
        assert_eq!(encoded(chord(KeyCode::Char('z'), KeyModifiers::CONTROL)), vec![0x1a]);
    }

    /// Terminals report Ctrl+Shift+C as an uppercase char; it is still the
    /// same control byte, not a literal 'C'.
    #[test]
    fn ctrl_is_case_insensitive() {
        let shifted = KeyModifiers::CONTROL | KeyModifiers::SHIFT;
        assert_eq!(encoded(chord(KeyCode::Char('C'), shifted)), vec![0x03]);
    }

    #[test]
    fn ctrl_chords_without_a_control_byte_are_dropped_not_sent_literally() {
        assert!(encode(chord(KeyCode::Char('.'), KeyModifiers::CONTROL), Modes::default()).is_none());
    }

    #[test]
    fn enter_sends_carriage_return_not_line_feed() {
        assert_eq!(encoded(key(KeyCode::Enter)), vec![0x0d]);
    }

    #[test]
    fn backspace_sends_delete_and_ctrl_backspace_sends_backspace() {
        assert_eq!(encoded(key(KeyCode::Backspace)), vec![0x7f]);
        assert_eq!(
            encoded(chord(KeyCode::Backspace, KeyModifiers::CONTROL)),
            vec![0x08]
        );
    }

    #[test]
    fn printable_characters_are_sent_as_utf8() {
        assert_eq!(encoded(key(KeyCode::Char('a'))), b"a".to_vec());
        assert_eq!(encoded(key(KeyCode::Char('ø'))), "ø".as_bytes().to_vec());
    }

    #[test]
    fn arrows_use_csi_normally_and_ss3_under_application_cursor_mode() {
        assert_eq!(encoded(key(KeyCode::Up)), b"\x1b[A".to_vec());
        let application = encode(key(KeyCode::Up), Modes::application_cursor(true)).unwrap();
        assert_eq!(application, b"\x1bOA".to_vec());
    }

    /// A modifier has nowhere to live in the SS3 form, so a modified arrow
    /// must fall back to the parameterized CSI form even in application
    /// cursor mode.
    #[test]
    fn modified_arrows_always_use_the_parameterized_csi_form() {
        let shift_up = encode(
            chord(KeyCode::Up, KeyModifiers::SHIFT),
            Modes::application_cursor(true),
        )
        .unwrap();
        assert_eq!(shift_up, b"\x1b[1;2A".to_vec());
        assert_eq!(
            encoded(chord(KeyCode::Left, KeyModifiers::CONTROL)),
            b"\x1b[1;5D".to_vec()
        );
    }

    #[test]
    fn home_end_and_the_tilde_family_get_their_escape_sequences() {
        assert_eq!(encoded(key(KeyCode::Home)), b"\x1b[H".to_vec());
        assert_eq!(encoded(key(KeyCode::End)), b"\x1b[F".to_vec());
        assert_eq!(encoded(key(KeyCode::Delete)), b"\x1b[3~".to_vec());
        assert_eq!(encoded(key(KeyCode::PageUp)), b"\x1b[5~".to_vec());
        assert_eq!(encoded(key(KeyCode::PageDown)), b"\x1b[6~".to_vec());
        assert_eq!(encoded(key(KeyCode::BackTab)), b"\x1b[Z".to_vec());
    }

    #[test]
    fn function_keys_split_between_the_ss3_and_tilde_families() {
        assert_eq!(encoded(key(KeyCode::F(1))), b"\x1bOP".to_vec());
        assert_eq!(encoded(key(KeyCode::F(4))), b"\x1bOS".to_vec());
        assert_eq!(encoded(key(KeyCode::F(5))), b"\x1b[15~".to_vec());
        assert_eq!(encoded(key(KeyCode::F(12))), b"\x1b[24~".to_vec());
        assert!(encode(key(KeyCode::F(20)), Modes::default()).is_none());
    }

    #[test]
    fn alt_prefixes_a_plain_key_with_escape_but_does_not_double_prefix_a_sequence() {
        assert_eq!(
            encoded(chord(KeyCode::Char('b'), KeyModifiers::ALT)),
            vec![0x1b, b'b']
        );
        assert_eq!(
            encoded(chord(KeyCode::Up, KeyModifiers::ALT)),
            b"\x1b[1;3A".to_vec()
        );
    }

    /// Key-release events would otherwise duplicate every keystroke on the
    /// terminals that report them.
    #[test]
    fn key_release_events_are_dropped() {
        let mut release = key(KeyCode::Char('a'));
        release.kind = KeyEventKind::Release;
        assert!(encode(release, Modes::default()).is_none());
    }
}
