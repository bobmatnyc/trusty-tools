//! The `tm ls` session actions, lent to the tui by the `tm` binary.
//!
//! #10754: S3 tm ls actions; De1 key map (Bob 11-06).
//!
//! Why: `tm ls` opens (resume + attach), renames, creates and deletes a
//! session through functions in the `tm` binary (`guided_resume`, `rename`,
//! `guided_launch`, `picker_delete`). The lib's `tui` cannot import the
//! binary, so the binary registers them here once at startup, the same way
//! [`crate::tui::focus`] takes its tmux switch.
//! What: [`SessionHooks`] holds one function pointer per operation; each
//! returns a boxed future of a [`SessionOpOutcome`]. [`set_session_hooks`]
//! registers them (first registration wins) and [`session_hooks`] reads them.
//! [`ScreenHandoff`] is how a caller lends the terminal out for the attach.
//! With no registration, a caller reports [`unavailable`] and runs nothing.
//! Test: `session_ops_without_hooks_report_unavailable`,
//! `session_ops_call_the_stub_hook_with_its_arguments` (`project_ctl`).

use std::future::Future;
use std::pin::Pin;
use std::sync::OnceLock;

use crate::client::DaemonClient;

/// What a session hook reports when it finishes without an error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionOpOutcome {
    /// The tui keeps running and shows this one-line notice.
    Done(String),
    /// The tmux hand-off moved the operator's client away from this pane
    /// (`switch-client`, or a fail-closed skip, #2678), so the tui stops, as
    /// `tm ls` does. The text is the notice for the frame nobody may see.
    LeftTerminal(String),
}

/// The future every hook returns.
pub type SessionOpFuture = Pin<Box<dyn Future<Output = anyhow::Result<SessionOpOutcome>> + Send>>;

/// The binary's `tm ls` session operations.
///
/// Why: one struct, registered once, so the binary cannot register some
/// operations and forget others: a struct literal must name every field.
/// What: each field wraps an existing `tm ls` function. Arguments after the
/// client are `id` for `open` / `delete`, `(id, new_name)` for `rename`, and
/// the project's registry `(name, repo_url)` for `new_session`.
/// Test: `tui_session_hooks_register_every_operation` (bin).
#[derive(Clone, Copy)]
pub struct SessionHooks {
    /// Resume the session if it needs it, then attach to it.
    pub open: fn(DaemonClient, String) -> SessionOpFuture,
    /// Rename the session to the given, already validated name.
    pub rename: fn(DaemonClient, String, String) -> SessionOpFuture,
    /// Start a new session in the registered project, then attach to it.
    pub new_session: fn(DaemonClient, String, String) -> SessionOpFuture,
    /// Delete the session by the route its state selects (`tm ls` `d`).
    pub delete: fn(DaemonClient, String) -> SessionOpFuture,
}

/// The hooks the `tm` binary registers once at startup.
static SESSION_HOOKS: OnceLock<SessionHooks> = OnceLock::new();

#[cfg(test)]
thread_local! {
    /// A test's stub hooks; per thread, so one test's stub never reaches
    /// another test, and the process-global [`SESSION_HOOKS`] stays unset.
    static TEST_HOOKS: std::cell::Cell<Option<SessionHooks>> =
        const { std::cell::Cell::new(None) };
}

/// Register the binary's session operations; the first registration wins.
///
/// Why: see the module doc. Returns `false` when hooks were already set.
pub fn set_session_hooks(hooks: SessionHooks) -> bool {
    SESSION_HOOKS.set(hooks).is_ok()
}

/// The registered hooks, if the binary set them.
///
/// In this crate's own tests, a thread's stub from `with_test_hooks` (test-only) wins.
pub fn session_hooks() -> Option<SessionHooks> {
    #[cfg(test)]
    if let Some(stub) = TEST_HOOKS.with(std::cell::Cell::get) {
        return Some(stub);
    }
    SESSION_HOOKS.get().copied()
}

/// Run `body` with `hooks` as this thread's hooks, then remove them.
///
/// Why: the global is a `OnceLock`, so a test cannot set and unset it; a
/// thread-local stub scoped to one test can. The stub is cleared even when
/// `body` panics.
#[cfg(test)]
pub(crate) async fn with_test_hooks<F: Future>(hooks: SessionHooks, body: F) -> F::Output {
    struct Clear;
    impl Drop for Clear {
        fn drop(&mut self) {
            TEST_HOOKS.with(|h| h.set(None));
        }
    }
    TEST_HOOKS.with(|h| h.set(Some(hooks)));
    let _clear = Clear;
    body.await
}

/// The notice for an operation no hook is registered for.
pub fn unavailable(verb: &str) -> String {
    format!("{verb} unavailable: no session hook registered (needs the `tm` binary)")
}

/// Lends the terminal to a child process and takes it back.
///
/// Why: open and new-session end in `tmux attach-session`, which needs a
/// cooked terminal on the main screen; the tui must hand it over first and
/// redraw after (#7224).
/// What: the real terminal maps these to [`crate::tui::terminal::suspend`] and
/// [`crate::tui::terminal::resume`]; a test records the calls.
/// Test: `open_suspends_and_resumes_around_the_hook`,
/// `open_resumes_the_terminal_when_the_hook_errors`.
pub trait ScreenHandoff {
    /// Give the real screen back in cooked mode.
    fn suspend(&mut self) -> std::io::Result<()>;
    /// Take the screen back and force a full redraw.
    fn resume(&mut self) -> std::io::Result<()>;
}
