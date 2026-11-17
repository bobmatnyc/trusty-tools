//! The launch form `l` opens for a project (#10754).
//!
//! See #10754 (owner ask ar-T5f, "tm ls launch").
//!
//! Why: DOC-35 §5.2 binds `l` to `launch` for the selected project, which
//! takes a task and an optional branch (`tm sessions launch --project <name>
//! --task "<text>" [--ref <branch>]`, §3.2). An empty task is refused here,
//! so no request reaches the daemon without one.
//! What: [`LaunchModal`] holds the project, the Task and Ref buffers and the
//! focused field. [`LaunchModal::apply`] takes one key and returns a
//! [`RenameStep`] (the rename modal's step type: ignore, redraw, cancel,
//! submit). A non-empty task on Enter becomes
//! [`PendingAction::Launch`] carrying a [`LaunchRequest`].
//! Test: `launch_modal_enter_submits_the_trimmed_task_and_ref`,
//! `launch_modal_refuses_an_empty_task`,
//! `launch_modal_ref_is_optional`,
//! `launch_modal_tab_switches_fields_and_backspace_edits_the_focused_one`,
//! `launch_modal_escape_cancels`.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::rename::RenameStep;
use crate::tui::project_ctl::events::PendingAction;

/// What a submitted launch form asks the daemon for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchRequest {
    /// Registry name of the project to launch in.
    pub project: String,
    /// The task, trimmed and non-empty.
    pub task: String,
    /// The branch or ref; `None` means the project's default branch.
    pub git_ref: Option<String>,
}

/// Which text field of the launch form takes typed keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchField {
    /// The required task text.
    Task,
    /// The optional branch or ref.
    Ref,
}

/// The message an Enter on an empty task shows.
pub const EMPTY_TASK: &str = "a task is required";

/// The open launch form's state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchModal {
    /// Registry name of the project the form launches in.
    pub project: String,
    /// The typed task.
    pub task: String,
    /// The typed branch or ref.
    pub git_ref: String,
    /// The field typed keys go to.
    pub field: LaunchField,
    /// The refusal from the last Enter, if any.
    pub error: Option<String>,
}

impl LaunchModal {
    /// Open the form on a project, both fields empty and Task focused.
    pub fn open(project: impl Into<String>) -> Self {
        Self {
            project: project.into(),
            task: String::new(),
            git_ref: String::new(),
            field: LaunchField::Task,
            error: None,
        }
    }

    /// The buffer of the focused field.
    fn focused(&mut self) -> &mut String {
        match self.field {
            LaunchField::Task => &mut self.task,
            LaunchField::Ref => &mut self.git_ref,
        }
    }

    /// Apply one key to the form.
    ///
    /// Why: see the module doc.
    /// What: a printable character appends to the focused field and Backspace
    /// pops from it (each clears the error); Tab, Shift+Tab, Up and Down
    /// switch fields; Esc cancels with "launch cancelled". Enter with a blank
    /// task keeps the form open with [`EMPTY_TASK`]; otherwise it submits the
    /// trimmed task and the trimmed ref (`None` when blank). Ctrl/Alt chords
    /// and other keys are ignored.
    /// Test: see the module doc.
    pub fn apply(&mut self, key: KeyEvent) -> RenameStep {
        match key.code {
            KeyCode::Esc => RenameStep::Cancel("launch cancelled".to_string()),
            KeyCode::Tab | KeyCode::BackTab | KeyCode::Up | KeyCode::Down => {
                self.field = match self.field {
                    LaunchField::Task => LaunchField::Ref,
                    LaunchField::Ref => LaunchField::Task,
                };
                RenameStep::Redraw
            }
            KeyCode::Backspace => {
                self.focused().pop();
                self.error = None;
                RenameStep::Redraw
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.focused().push(c);
                self.error = None;
                RenameStep::Redraw
            }
            KeyCode::Enter => match self.request() {
                Some(request) => RenameStep::Submit(PendingAction::Launch(request)),
                None => {
                    self.error = Some(EMPTY_TASK.to_string());
                    RenameStep::Redraw
                }
            },
            _ => RenameStep::Ignore,
        }
    }

    /// The request this form submits, or `None` while the task is blank.
    pub fn request(&self) -> Option<LaunchRequest> {
        let task = self.task.trim();
        if task.is_empty() {
            return None;
        }
        let git_ref = self.git_ref.trim();
        Some(LaunchRequest {
            project: self.project.clone(),
            task: task.to_string(),
            git_ref: (!git_ref.is_empty()).then(|| git_ref.to_string()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn type_str(modal: &mut LaunchModal, text: &str) {
        for c in text.chars() {
            assert_eq!(modal.apply(key(KeyCode::Char(c))), RenameStep::Redraw);
        }
    }

    fn submitted(modal: &mut LaunchModal) -> LaunchRequest {
        match modal.apply(key(KeyCode::Enter)) {
            RenameStep::Submit(PendingAction::Launch(request)) => request,
            other => panic!("expected a launch submit, got {other:?}"),
        }
    }

    #[test]
    fn launch_modal_enter_submits_the_trimmed_task_and_ref() {
        let mut modal = LaunchModal::open("alpha");
        type_str(&mut modal, "  fix the flaky test ");
        modal.apply(key(KeyCode::Tab));
        type_str(&mut modal, " feat/x ");
        assert_eq!(
            submitted(&mut modal),
            LaunchRequest {
                project: "alpha".to_string(),
                task: "fix the flaky test".to_string(),
                git_ref: Some("feat/x".to_string()),
            }
        );
    }

    #[test]
    fn launch_modal_refuses_an_empty_task() {
        let mut modal = LaunchModal::open("alpha");
        type_str(&mut modal, "   ");
        modal.apply(key(KeyCode::Tab));
        type_str(&mut modal, "main");
        assert_eq!(modal.apply(key(KeyCode::Enter)), RenameStep::Redraw);
        assert_eq!(modal.error.as_deref(), Some(EMPTY_TASK));
        // Typing again clears the refusal.
        type_str(&mut modal, "x");
        assert_eq!(modal.error, None);
    }

    #[test]
    fn launch_modal_ref_is_optional() {
        let mut modal = LaunchModal::open("alpha");
        type_str(&mut modal, "write docs");
        modal.apply(key(KeyCode::Down));
        type_str(&mut modal, "  ");
        assert_eq!(submitted(&mut modal).git_ref, None);
    }

    #[test]
    fn launch_modal_tab_switches_fields_and_backspace_edits_the_focused_one() {
        let mut modal = LaunchModal::open("alpha");
        type_str(&mut modal, "ab");
        modal.apply(key(KeyCode::Tab));
        assert_eq!(modal.field, LaunchField::Ref);
        type_str(&mut modal, "cd");
        assert_eq!(modal.apply(key(KeyCode::Backspace)), RenameStep::Redraw);
        assert_eq!((modal.task.as_str(), modal.git_ref.as_str()), ("ab", "c"));
        modal.apply(key(KeyCode::BackTab));
        assert_eq!(modal.field, LaunchField::Task);
        let ctrl = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL);
        assert_eq!(modal.apply(ctrl), RenameStep::Ignore);
        assert_eq!(modal.task, "ab");
    }

    #[test]
    fn launch_modal_escape_cancels() {
        let mut modal = LaunchModal::open("alpha");
        type_str(&mut modal, "task");
        assert_eq!(
            modal.apply(key(KeyCode::Esc)),
            RenameStep::Cancel("launch cancelled".to_string())
        );
    }
}
