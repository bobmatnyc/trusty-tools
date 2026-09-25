//! The `tm ls` new-session flow's name step (#8587).
//!
//! Why: Enter on a project creates a session with the default name
//! (`tm-<project>-NN`), and the TUI had no way to name one before it existed.
//! Ctrl-N on the selected project opens this step instead; Enter here creates
//! the session under the typed name, Esc goes back to the project list.
//!
//! What this REUSES: the typed text is slugged by
//! [`leaf_slug_from_hint`] — the same function and cap the numbered picker's
//! `n <name>` uses — so the daemon's own pass over `name_hint` is a no-op and
//! the preview shown here is the leaf the session gets. The project half of the
//! request comes from
//! [`request_for_registered`](super::new_session::request_for_registered), the
//! builder Enter uses, so Ctrl-N cannot accept a row Enter would refuse.
//!
//! Test: `new_session_name_*` in `super::tests`.

use trusty_common::session_naming::{PREFIX, leaf_slug_from_hint};

use super::new_session::{NewSessionRequest, Step, Target, request_for_registered};
use super::state::Input;

/// Why Ctrl-N refuses the "type a path" row: it has no project to name yet.
pub(crate) const NOT_A_PROJECT: &str =
    "Ctrl-N names a session in a registered project — pick one, or press Enter to type a path";

/// The name step's own state: the project already chosen, and the typed name.
///
/// Why: the project half of the request is settled when the step opens, so the
/// step only has to own the text. Holding the finished request is what lets
/// Enter here differ from Enter on the list by exactly one field.
/// What: `request` is the default-named request Enter on the row would have
/// produced; `typed` is the raw text, slugged only when it is read.
/// Test: `new_session_name_enter_creates_with_the_slug`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NameStep {
    request: NewSessionRequest,
    typed: String,
}

/// Open the name step on the selected row, or say why it cannot open.
///
/// Why: a row Enter would refuse must be refused at the Ctrl-N keypress too,
/// before the operator types a name that could never be used.
/// What: a registered row goes through [`request_for_registered`]; on success
/// `slot` holds the new step. Its refusal (no project directory, no checkout on
/// this host) is returned as [`Step::Reject`] with that function's own message.
/// The escape row is refused with [`NOT_A_PROJECT`]. No row is
/// [`Step::Ignore`]. A refusal leaves `slot` empty.
/// Test: `new_session_name_ctrl_n_opens_the_name_step`,
/// `new_session_name_ctrl_n_rejects_the_path_row`,
/// `new_session_name_ctrl_n_rejects_a_row_without_a_checkout`.
pub(crate) fn open(slot: &mut Option<NameStep>, target: Option<&Target>) -> Step {
    let (name, repo, checkout) = match target {
        Some(Target::Registered {
            name,
            repo,
            checkout,
        }) => (name, repo, checkout),
        Some(Target::Other) => return Step::Reject(NOT_A_PROJECT.to_string()),
        None => return Step::Ignore,
    };
    match request_for_registered(name, repo, checkout.as_deref()) {
        Ok(request) => {
            *slot = Some(NameStep {
                request,
                typed: String::new(),
            });
            Step::Redraw
        }
        Err(message) => Step::Reject(message),
    }
}

/// Give an open name step the keystroke, before the project list sees it.
///
/// Why: the list's Esc arm clears a non-empty filter first; routing the name
/// step ahead of it is what makes Esc here close only the step.
/// What: `None` when no step is open, so the list handles the key. Otherwise
/// the step's own [`Step`], except that its [`Step::Cancel`] closes the step
/// and becomes [`Step::Redraw`].
/// Test: `new_session_name_escape_returns_to_the_list_intact`.
pub(crate) fn route(slot: &mut Option<NameStep>, input: Input) -> Option<Step> {
    let step = slot.as_mut()?.apply(input);
    if step == Step::Cancel {
        *slot = None;
        return Some(Step::Redraw);
    }
    Some(step)
}

impl NameStep {
    /// The text typed so far, unslugged.
    pub(crate) fn typed(&self) -> &str {
        &self.typed
    }

    /// The project the session will be created in.
    pub(crate) fn label(&self) -> &str {
        &self.request.label
    }

    /// The session name the typed text produces, as `tm-<slug>-NN`.
    ///
    /// Why: the slug drops case, spaces and symbols, so the operator needs to
    /// see the result before Enter rather than after.
    /// What: `None` while the slug is empty. `NN` stands for the per-name
    /// serial the daemon assigns.
    /// Test: `new_session_name_preview_shows_the_slug`.
    pub(crate) fn preview(&self) -> Option<String> {
        let slug = leaf_slug_from_hint(&self.typed);
        (!slug.is_empty()).then(|| format!("{PREFIX}{slug}-NN"))
    }

    /// Route one keystroke through the name step.
    ///
    /// Why: Esc here must leave the step, not the flow and not the filter; the
    /// caller reads [`Step::Cancel`] as "back to the project list".
    /// What: characters and Backspace edit the text. Enter slugs it: a
    /// non-empty slug is [`Step::Create`] with `name_hint` set, an empty one is
    /// [`Step::Reject`] and the step stays open.
    /// Test: `new_session_name_enter_creates_with_the_slug`,
    /// `new_session_name_rejects_an_empty_slug`,
    /// `new_session_name_escape_returns_to_the_list_intact`.
    pub(crate) fn apply(&mut self, input: Input) -> Step {
        match input {
            Input::Escape => Step::Cancel,
            Input::Char(c) => {
                self.typed.push(c);
                Step::Redraw
            }
            Input::Backspace => match self.typed.pop() {
                Some(_) => Step::Redraw,
                None => Step::Ignore,
            },
            Input::Enter => {
                let slug = leaf_slug_from_hint(&self.typed);
                if slug.is_empty() {
                    return Step::Reject(format!(
                        "'{}' has no letters or digits to name a session with — \
                         type a name, or Esc to go back",
                        self.typed.trim()
                    ));
                }
                Step::Create(NewSessionRequest {
                    name_hint: Some(slug),
                    ..self.request.clone()
                })
            }
            _ => Step::Ignore,
        }
    }
}

/// The status line after a create, naming the session when it was named.
///
/// Why: the daemon picks the serial, and the TUI does not read it back; the
/// `tm-<slug>-NN` form is the part the operator chose.
/// What: `new session in <project>` for an unnamed request, and
/// `new session tm-<slug>-NN in <project>` for a named one.
/// Test: `new_session_name_status_line_names_the_session`.
pub(crate) fn created_message(request: &NewSessionRequest) -> String {
    match request.name_hint.as_deref() {
        Some(slug) => format!("new session {PREFIX}{slug}-NN in {}", request.label),
        None => format!("new session in {}", request.label),
    }
}
