//! #10754: S3 tm ls actions plumbing (keys wait on De1) — the hook dispatch.
//!
//! The stubs are plain `fn`s (the hook type), so they record into a
//! thread-local log; the screen records into the same log, which is what
//! proves suspend, hook and resume run in that order. `#[tokio::test]` runs on
//! one thread, and `with_test_hooks` scopes each stub to its own test.

use std::cell::RefCell;

use super::actions::dispatch_on;
use super::events::PendingAction;
use super::state::{ProjectCtlState, ProjectRow};
use crate::client::DaemonClient;
use crate::tui::session_hooks::{
    ScreenHandoff, SessionHooks, SessionOpFuture, SessionOpOutcome, unavailable, with_test_hooks,
};

thread_local! {
    static LOG: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

fn log(entry: String) {
    LOG.with(|l| l.borrow_mut().push(entry));
}

fn take_log() -> Vec<String> {
    LOG.with(|l| std::mem::take(&mut *l.borrow_mut()))
}

/// A screen that records its hand-off calls.
struct Recorder;

impl ScreenHandoff for Recorder {
    fn suspend(&mut self) -> std::io::Result<()> {
        log("suspend".to_string());
        Ok(())
    }

    fn resume(&mut self) -> std::io::Result<()> {
        log("resume".to_string());
        Ok(())
    }
}

fn done(entry: String) -> SessionOpFuture {
    Box::pin(async move {
        log(entry.clone());
        Ok(SessionOpOutcome::Done(format!("did {entry}")))
    })
}

fn open_ok(_: DaemonClient, id: String) -> SessionOpFuture {
    done(format!("open:{id}"))
}

fn rename_ok(_: DaemonClient, id: String, name: String) -> SessionOpFuture {
    done(format!("rename:{id}:{name}"))
}

fn new_ok(_: DaemonClient, name: String, repo_url: String) -> SessionOpFuture {
    done(format!("new:{name}:{repo_url}"))
}

fn delete_ok(_: DaemonClient, id: String) -> SessionOpFuture {
    done(format!("delete:{id}"))
}

fn open_err(_: DaemonClient, id: String) -> SessionOpFuture {
    Box::pin(async move {
        log(format!("open:{id}"));
        Err(anyhow::anyhow!("boom"))
    })
}

fn open_left(_: DaemonClient, id: String) -> SessionOpFuture {
    Box::pin(async move {
        log(format!("open:{id}"));
        Ok(SessionOpOutcome::LeftTerminal("switched".to_string()))
    })
}

const STUBS: SessionHooks = SessionHooks {
    open: open_ok,
    rename: rename_ok,
    new_session: new_ok,
    delete: delete_ok,
};

fn state() -> ProjectCtlState {
    ProjectCtlState {
        projects: vec![ProjectRow {
            name: "alpha".to_string(),
            repo_url: "https://github.com/acme/alpha".to_string(),
            ..Default::default()
        }],
        ..Default::default()
    }
}

fn client() -> DaemonClient {
    // Never contacted: every call below goes to a stub or to nothing.
    DaemonClient::new("http://127.0.0.1:9")
}

/// Dispatch one action against `state` with a recording screen.
async fn run(state: &mut ProjectCtlState, action: PendingAction) {
    dispatch_on(state, &client(), action, &mut Recorder)
        .await
        .expect("the recorder never fails a resume");
}

fn four_actions() -> Vec<(PendingAction, &'static str)> {
    vec![
        (PendingAction::OpenSession("s1".into()), "open"),
        (
            PendingAction::RenameSession("s1".into(), "n".into()),
            "rename",
        ),
        (PendingAction::NewSession("alpha".into()), "new session"),
        (PendingAction::DeleteSession("s1".into()), "delete"),
    ]
}

#[tokio::test]
async fn session_ops_without_hooks_report_unavailable() {
    take_log();
    for (action, verb) in four_actions() {
        let mut state = state();
        run(&mut state, action).await;
        assert_eq!(state.notice.as_deref(), Some(unavailable(verb).as_str()));
        assert!(!state.take_repoll(), "{verb}: nothing ran, so no repoll");
    }
    assert!(take_log().is_empty(), "no hook and no hand-off may run");
}

#[tokio::test]
async fn session_ops_call_the_stub_hook_with_its_arguments() {
    take_log();
    with_test_hooks(STUBS, async {
        let expected = [
            vec!["suspend", "open:s1", "resume"],
            vec!["rename:s1:n"],
            vec![
                "suspend",
                "new:alpha:https://github.com/acme/alpha",
                "resume",
            ],
            vec!["delete:s1"],
        ];
        for ((action, verb), want) in four_actions().into_iter().zip(expected) {
            let mut state = state();
            run(&mut state, action).await;
            let got = take_log();
            assert_eq!(got, want, "{verb}");
            assert_eq!(state.notice, Some(format!("did {}", want[want.len() / 2])));
            assert!(state.take_repoll(), "{verb} must repoll");
            assert!(!state.should_exit);
        }
    })
    .await;
}

#[tokio::test]
async fn open_suspends_and_resumes_around_the_hook() {
    take_log();
    with_test_hooks(STUBS, async {
        let mut state = state();
        run(&mut state, PendingAction::OpenSession("s9".into())).await;
        assert_eq!(take_log(), ["suspend", "open:s9", "resume"]);
    })
    .await;
}

#[tokio::test]
async fn open_resumes_the_terminal_when_the_hook_errors() {
    take_log();
    let hooks = SessionHooks {
        open: open_err,
        ..STUBS
    };
    with_test_hooks(hooks, async {
        let mut state = state();
        run(&mut state, PendingAction::OpenSession("s1".into())).await;
        assert_eq!(take_log(), ["suspend", "open:s1", "resume"]);
        assert_eq!(state.notice.as_deref(), Some("open failed: boom"));
        assert!(!state.should_exit);
    })
    .await;
}

#[tokio::test]
async fn left_terminal_outcome_asks_the_loop_to_exit() {
    take_log();
    let hooks = SessionHooks {
        open: open_left,
        ..STUBS
    };
    with_test_hooks(hooks, async {
        let mut state = state();
        run(&mut state, PendingAction::OpenSession("s1".into())).await;
        assert_eq!(take_log(), ["suspend", "open:s1", "resume"]);
        assert_eq!(state.notice.as_deref(), Some("switched"));
        assert!(state.should_exit, "tm ls stops after a switch-client");
    })
    .await;
}

#[tokio::test]
async fn new_session_in_an_unknown_project_runs_no_hook() {
    take_log();
    with_test_hooks(STUBS, async {
        let mut state = state();
        run(&mut state, PendingAction::NewSession("ghost".into())).await;
        assert!(take_log().is_empty());
        assert_eq!(
            state.notice.as_deref(),
            Some("new session failed: project 'ghost' is not registered")
        );
    })
    .await;
}
