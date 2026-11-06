//! The rename text-entry modal for a session row.
//!
//! #10754: De1 key map (Bob 11-06) — `r` on a session row opens it through
//! [`RenameModal::prefilled`], with the current name in the buffer.
//!
//! Why: `tm ls` renames a session from a typed name, checked by the same
//! `validate_session_name` the daemon runs, so a bad name is refused at Enter
//! instead of after a round trip. The tui needs the same entry.
//! What: [`RenameModal`] holds the session id, its current name and the typed
//! buffer. [`RenameModal::apply`] takes one key and returns a [`RenameStep`];
//! a valid, changed name on Enter becomes
//! [`PendingAction::RenameSession`] with the trimmed name.
//! Test: `rename_modal_prefilled_starts_from_the_current_name`,
//! `rename_modal_types_and_backspaces`,
//! `rename_modal_enter_submits_the_trimmed_validated_name`,
//! `rename_modal_enter_refuses_an_invalid_name`,
//! `rename_modal_enter_on_the_unchanged_name_cancels`,
//! `rename_modal_escape_cancels`.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::session_manager::rename::validate_session_name;
use crate::tui::project_ctl::events::PendingAction;

/// The open rename modal's state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenameModal {
    /// Full id of the session being renamed.
    pub session_id: String,
    /// The session's name when the modal opened.
    pub current_name: String,
    /// What the operator has typed so far.
    pub typed: String,
    /// The validator's message from the last refused Enter, if any.
    pub error: Option<String>,
}

/// What one key did to the modal.
#[derive(Debug, Clone, PartialEq)]
pub enum RenameStep {
    /// Nothing changed.
    Ignore,
    /// The buffer or error changed; redraw.
    Redraw,
    /// Close the modal with no action, showing this notice.
    Cancel(String),
    /// Close the modal and run this action.
    Submit(PendingAction),
}

impl RenameModal {
    /// Open the modal on a session, with an empty buffer.
    pub fn open(session_id: impl Into<String>, current_name: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            current_name: current_name.into(),
            typed: String::new(),
            error: None,
        }
    }

    /// Open the modal with the session's current name already typed.
    pub fn prefilled(session_id: impl Into<String>, current_name: impl Into<String>) -> Self {
        let mut modal = Self::open(session_id, current_name);
        modal.typed = modal.current_name.clone();
        modal
    }

    /// Apply one key, as `tm ls`'s rename overlay does.
    ///
    /// Why: see the module doc.
    /// What: a printable character appends and Backspace pops (each clears
    /// the error); Esc cancels. Enter runs `validate_session_name`: an invalid
    /// name keeps the modal open with the message; the unchanged name
    /// cancels with "name unchanged"; any other name submits
    /// `RenameSession(id, trimmed)`. Ctrl/Alt chords and other keys are ignored.
    /// Test: see the module doc.
    pub fn apply(&mut self, key: KeyEvent) -> RenameStep {
        match key.code {
            KeyCode::Esc => RenameStep::Cancel("rename cancelled".to_string()),
            KeyCode::Backspace => {
                self.typed.pop();
                self.error = None;
                RenameStep::Redraw
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.typed.push(c);
                self.error = None;
                RenameStep::Redraw
            }
            KeyCode::Enter => match validate_session_name(&self.typed) {
                Err(msg) => {
                    self.error = Some(msg);
                    RenameStep::Redraw
                }
                Ok(name) if name == self.current_name => {
                    RenameStep::Cancel("name unchanged".to_string())
                }
                Ok(name) => {
                    RenameStep::Submit(PendingAction::RenameSession(self.session_id.clone(), name))
                }
            },
            _ => RenameStep::Ignore,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn type_str(modal: &mut RenameModal, text: &str) {
        for c in text.chars() {
            assert_eq!(modal.apply(key(KeyCode::Char(c))), RenameStep::Redraw);
        }
    }

    #[test]
    fn rename_modal_prefilled_starts_from_the_current_name() {
        let mut modal = RenameModal::prefilled("id-1", "old");
        assert_eq!(modal.typed, "old");
        type_str(&mut modal, "-2");
        assert_eq!(
            modal.apply(key(KeyCode::Enter)),
            RenameStep::Submit(PendingAction::RenameSession(
                "id-1".to_string(),
                "old-2".to_string()
            ))
        );
    }

    #[test]
    fn rename_modal_types_and_backspaces() {
        let mut modal = RenameModal::open("id-1", "old");
        type_str(&mut modal, "abc");
        assert_eq!(modal.apply(key(KeyCode::Backspace)), RenameStep::Redraw);
        assert_eq!(modal.typed, "ab");
        let ctrl = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL);
        assert_eq!(modal.apply(ctrl), RenameStep::Ignore);
        assert_eq!(modal.typed, "ab");
    }

    #[test]
    fn rename_modal_enter_submits_the_trimmed_validated_name() {
        let mut modal = RenameModal::open("id-1", "old");
        type_str(&mut modal, "  new-name ");
        assert_eq!(
            modal.apply(key(KeyCode::Enter)),
            RenameStep::Submit(PendingAction::RenameSession(
                "id-1".to_string(),
                "new-name".to_string()
            ))
        );
    }

    #[test]
    fn rename_modal_enter_refuses_an_invalid_name() {
        let mut modal = RenameModal::open("id-1", "old");
        assert_eq!(modal.apply(key(KeyCode::Enter)), RenameStep::Redraw);
        let msg = modal.error.clone().expect("an empty name is refused");
        assert_eq!(Err(msg), validate_session_name(""));
        // Typing again clears the refusal.
        type_str(&mut modal, "a");
        assert_eq!(modal.error, None);
    }

    #[test]
    fn rename_modal_enter_on_the_unchanged_name_cancels() {
        let mut modal = RenameModal::open("id-1", "old");
        type_str(&mut modal, "old");
        assert_eq!(
            modal.apply(key(KeyCode::Enter)),
            RenameStep::Cancel("name unchanged".to_string())
        );
    }

    #[test]
    fn rename_modal_escape_cancels() {
        let mut modal = RenameModal::open("id-1", "old");
        type_str(&mut modal, "new");
        assert_eq!(
            modal.apply(key(KeyCode::Esc)),
            RenameStep::Cancel("rename cancelled".to_string())
        );
    }
}
