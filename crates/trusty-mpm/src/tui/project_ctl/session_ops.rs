//! Dispatch of the `tm ls` session actions through the binary's hooks.
//!
//! #10754: S3 tm ls actions; De1 key map (Bob 11-06).
//!
//! Why: open, rename, new and delete must do in the tui exactly what they do
//! in `tm ls`, and that code lives in the `tm` binary; see
//! [`crate::tui::session_hooks`].
//! What: [`run`] looks the hooks up, hands the terminal over around the two
//! operations that attach (open, new), and folds the outcome into a notice.
//! Test: `session_ops_tests`.

use crate::client::DaemonClient;
use crate::tui::session_hooks::{
    ScreenHandoff, SessionOpFuture, SessionOpOutcome, session_hooks, unavailable,
};

use super::state::ProjectCtlState;

/// One `tm ls` session operation, as the dispatcher received it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SessionOp {
    /// Open the session with this id.
    Open(String),
    /// Rename the session `(id, new_name)`.
    Rename(String, String),
    /// Start a new session in the named project.
    New(String),
    /// Delete the session with this id.
    Delete(String),
}

impl SessionOp {
    /// The verb the notices name.
    fn verb(&self) -> &'static str {
        match self {
            SessionOp::Open(_) => "open",
            SessionOp::Rename(..) => "rename",
            SessionOp::New(_) => "new session",
            SessionOp::Delete(_) => "delete",
        }
    }

    /// Does this operation end in a tmux attach that needs the terminal?
    fn attaches(&self) -> bool {
        matches!(self, SessionOp::Open(_) | SessionOp::New(_))
    }
}

/// Run one session operation through the registered hook.
///
/// Why: the single seam the tui's dispatcher calls for the `tm ls` verbs.
/// What: with no hook registered, sets the [`unavailable`] notice and runs
/// nothing. A new session in a project the poll does not know is a notice.
/// Otherwise: for open and new, [`ScreenHandoff::suspend`] first, then the
/// hook, then [`ScreenHandoff::resume`] whatever the hook returned. A failed
/// suspend skips the hook and still resumes. `Done` sets its notice;
/// `LeftTerminal` sets its notice and asks the loop to exit, as `tm ls` stops
/// after a `switch-client`; an error sets `"{verb} failed: {e}"`. Every run of
/// a hook requests a repoll. Only a failed resume is returned as an error,
/// since the terminal can no longer be drawn.
/// Test: `session_ops_without_hooks_report_unavailable`,
/// `session_ops_call_the_stub_hook_with_its_arguments`,
/// `open_suspends_and_resumes_around_the_hook`,
/// `open_resumes_the_terminal_when_the_hook_errors`,
/// `left_terminal_outcome_asks_the_loop_to_exit`.
pub(crate) async fn run(
    state: &mut ProjectCtlState,
    client: &DaemonClient,
    op: SessionOp,
    screen: &mut dyn ScreenHandoff,
) -> anyhow::Result<()> {
    let verb = op.verb();
    let Some(hooks) = session_hooks() else {
        state.set_notice(unavailable(verb));
        return Ok(());
    };
    let attaches = op.attaches();
    let future: SessionOpFuture = match op {
        SessionOp::Open(id) => (hooks.open)(client.clone(), id),
        SessionOp::Rename(id, name) => (hooks.rename)(client.clone(), id, name),
        SessionOp::Delete(id) => (hooks.delete)(client.clone(), id),
        SessionOp::New(project) => {
            let Some(row) = state.projects.iter().find(|p| p.name == project) else {
                state.set_notice(format!(
                    "{verb} failed: project '{project}' is not registered"
                ));
                return Ok(());
            };
            (hooks.new_session)(client.clone(), row.name.clone(), row.repo_url.clone())
        }
    };

    if attaches && let Err(e) = screen.suspend() {
        state.set_notice(format!(
            "{verb} failed: could not hand over the terminal: {e}"
        ));
        screen.resume()?;
        return Ok(());
    }
    let result = future.await;
    let resumed = if attaches { screen.resume() } else { Ok(()) };

    match result {
        Ok(SessionOpOutcome::Done(text)) => state.set_notice(text),
        Ok(SessionOpOutcome::LeftTerminal(text)) => {
            state.set_notice(text);
            state.should_exit = true;
        }
        Err(e) => state.set_notice(format!("{verb} failed: {e:#}")),
    }
    state.request_repoll();
    resumed.map_err(Into::into)
}

/// A hand-off that does nothing, for a caller with no terminal (tests).
#[cfg(test)]
pub(crate) struct NoHandoff;

#[cfg(test)]
impl ScreenHandoff for NoHandoff {
    fn suspend(&mut self) -> std::io::Result<()> {
        Ok(())
    }

    fn resume(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
