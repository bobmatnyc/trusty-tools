//! The `crossterm` → [`KeyInput`] translation boundary (Slice 2, #3414).
//!
//! Why: Slice 1 (#3412) defined [`KeyInput`]/[`KeyCode`]/[`KeyModifiers`] as a
//! deliberately `crossterm`-free vocabulary so `ReplEvent` (and every
//! `TuiEngine` consumer) never needs the `crossterm` dependency — only the
//! terminal layer itself does. This module is that one translation point:
//! [`spawn_key_reader`] in [`crate::run`] calls [`translate_key_event`]
//! exactly once, at the moment a real `crossterm::event::KeyEvent` comes off
//! the blocking reader thread, so every line of code past that point (the
//! shared event loop, both `TuiEngine` implementations, and eventually the
//! Slice 4 widgets) only ever sees [`KeyInput`].
//!
//! What: a pure, allocation-free mapping from `crossterm::event::{KeyCode,
//! KeyModifiers}` to their [`crate::event`] counterparts. `KeyCode::Other`
//! (Slice 1's deliberate catch-all) absorbs every crossterm variant the line
//! editor doesn't switch on (F-keys, media keys, `KeyCode::Null`, …) so this
//! mapping never needs to change shape just because crossterm adds a new key
//! variant upstream.
//!
//! # Spec References
//! - [`SPEC-TTUI-05~draft`](../../../docs/specs/DOC-50-tcode-tui-claude-code-clone.md#SPEC-TTUI-05~draft) — Slice 2 deliverable (§5, Slice 2).

use crossterm::event::{KeyCode as CtKeyCode, KeyEvent, KeyModifiers as CtKeyModifiers};

use crate::event::{KeyCode, KeyInput, KeyModifiers};

/// Translate one `crossterm::event::KeyEvent` into a [`KeyInput`].
///
/// Why: the sole call site is [`crate::run::spawn_key_reader`]'s blocking
/// read loop — see that module's doc comment for why the translation has to
/// happen there rather than downstream.
/// What: delegates to [`translate_key_code`] and [`translate_modifiers`];
/// kept as a thin combinator so each half is independently unit-testable.
/// Test: [`tests::translate_key_event_combines_code_and_modifiers`].
pub fn translate_key_event(ev: KeyEvent) -> KeyInput {
    KeyInput {
        code: translate_key_code(ev.code),
        modifiers: translate_modifiers(ev.modifiers),
    }
}

/// Translate a `crossterm::event::KeyCode` into a [`KeyCode`].
///
/// Why: isolates the (long, mechanical) variant-by-variant mapping so
/// [`translate_key_event`] reads as a two-line combinator.
/// What: maps every [`KeyCode`] variant Slice 1 defined to its crossterm
/// counterpart; anything else (F-keys, media keys, `Null`, …) falls through
/// to [`KeyCode::Other`] by design (see the module doc comment).
/// Test: [`tests::translate_key_code_maps_named_keys`],
/// [`tests::translate_key_code_maps_printable_char`],
/// [`tests::translate_key_code_falls_back_to_other`].
fn translate_key_code(code: CtKeyCode) -> KeyCode {
    match code {
        CtKeyCode::Char(c) => KeyCode::Char(c),
        CtKeyCode::Enter => KeyCode::Enter,
        CtKeyCode::Backspace => KeyCode::Backspace,
        CtKeyCode::Delete => KeyCode::Delete,
        CtKeyCode::Tab => KeyCode::Tab,
        CtKeyCode::Esc => KeyCode::Esc,
        CtKeyCode::Left => KeyCode::Left,
        CtKeyCode::Right => KeyCode::Right,
        CtKeyCode::Up => KeyCode::Up,
        CtKeyCode::Down => KeyCode::Down,
        CtKeyCode::Home => KeyCode::Home,
        CtKeyCode::End => KeyCode::End,
        CtKeyCode::PageUp => KeyCode::PageUp,
        CtKeyCode::PageDown => KeyCode::PageDown,
        _ => KeyCode::Other,
    }
}

/// Translate `crossterm::event::KeyModifiers` bitflags into [`KeyModifiers`].
///
/// Why: `KeyInput`'s line editor (tagent's `keys.rs` today, Slice 4's shared
/// widget tomorrow) only ever switches on ctrl/alt/shift, so that's all this
/// carries forward — matches Slice 1's [`KeyModifiers`] doc comment.
/// What: three independent `contains()` checks; crossterm's `SUPER`/`HYPER`/
/// `META` bits are intentionally dropped (no consumer reads them).
/// Test: [`tests::translate_modifiers_reads_ctrl_alt_shift_independently`].
fn translate_modifiers(m: CtKeyModifiers) -> KeyModifiers {
    KeyModifiers {
        ctrl: m.contains(CtKeyModifiers::CONTROL),
        alt: m.contains(CtKeyModifiers::ALT),
        shift: m.contains(CtKeyModifiers::SHIFT),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEventKind, KeyEventState};

    fn key_event(code: CtKeyCode, modifiers: CtKeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    #[test]
    fn translate_key_code_maps_named_keys() {
        assert_eq!(translate_key_code(CtKeyCode::Enter), KeyCode::Enter);
        assert_eq!(translate_key_code(CtKeyCode::Backspace), KeyCode::Backspace);
        assert_eq!(translate_key_code(CtKeyCode::Delete), KeyCode::Delete);
        assert_eq!(translate_key_code(CtKeyCode::Tab), KeyCode::Tab);
        assert_eq!(translate_key_code(CtKeyCode::Esc), KeyCode::Esc);
        assert_eq!(translate_key_code(CtKeyCode::Left), KeyCode::Left);
        assert_eq!(translate_key_code(CtKeyCode::Right), KeyCode::Right);
        assert_eq!(translate_key_code(CtKeyCode::Up), KeyCode::Up);
        assert_eq!(translate_key_code(CtKeyCode::Down), KeyCode::Down);
        assert_eq!(translate_key_code(CtKeyCode::Home), KeyCode::Home);
        assert_eq!(translate_key_code(CtKeyCode::End), KeyCode::End);
        assert_eq!(translate_key_code(CtKeyCode::PageUp), KeyCode::PageUp);
        assert_eq!(translate_key_code(CtKeyCode::PageDown), KeyCode::PageDown);
    }

    #[test]
    fn translate_key_code_maps_printable_char() {
        assert_eq!(translate_key_code(CtKeyCode::Char('a')), KeyCode::Char('a'));
        assert_eq!(translate_key_code(CtKeyCode::Char('Z')), KeyCode::Char('Z'));
    }

    #[test]
    fn translate_key_code_falls_back_to_other() {
        assert_eq!(translate_key_code(CtKeyCode::F(5)), KeyCode::Other);
        assert_eq!(translate_key_code(CtKeyCode::Null), KeyCode::Other);
        assert_eq!(translate_key_code(CtKeyCode::Insert), KeyCode::Other);
    }

    #[test]
    fn translate_modifiers_reads_ctrl_alt_shift_independently() {
        let none = translate_modifiers(CtKeyModifiers::NONE);
        assert!(!none.ctrl && !none.alt && !none.shift);

        let ctrl = translate_modifiers(CtKeyModifiers::CONTROL);
        assert!(ctrl.ctrl && !ctrl.alt && !ctrl.shift);

        let ctrl_shift = translate_modifiers(CtKeyModifiers::CONTROL | CtKeyModifiers::SHIFT);
        assert!(ctrl_shift.ctrl && !ctrl_shift.alt && ctrl_shift.shift);

        // crossterm's SUPER bit is intentionally dropped — must not be
        // misread as any of the three bits this type carries.
        let sup = translate_modifiers(CtKeyModifiers::SUPER);
        assert!(!sup.ctrl && !sup.alt && !sup.shift);
    }

    #[test]
    fn translate_key_event_combines_code_and_modifiers() {
        let ev = key_event(CtKeyCode::Char('c'), CtKeyModifiers::CONTROL);
        let input = translate_key_event(ev);
        assert_eq!(input.code, KeyCode::Char('c'));
        assert!(input.modifiers.ctrl);
        assert!(!input.modifiers.alt);
        assert!(!input.modifiers.shift);
    }
}
