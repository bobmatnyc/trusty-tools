//! Tests for `ta handoff` (#11650, slice 3c).
//!
//! Why: no test may reach a real tm daemon or tcode. The flow runs against
//! [`FakeTm`] and [`FakeTc`] at the client boundary; the real tc client runs
//! against the crate's in-process mock socket (`crate::uds_mock`).
//! What: the shared fakes and a temp instance home; the tests live in the
//! submodules.
//! Test: this module IS the test surface.

mod args_tests;
mod client_tests;
mod flow_tests;
mod ledger_tests;

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;

use super::{CallError, TcClient, TmClient, TmSessionState};
use crate::assistants::{AssistantHome, AssistantInstanceId};

/// One call a fake saw, with its exact arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Seen {
    New {
        repo: PathBuf,
        task: String,
    },
    State {
        id: String,
    },
    Send {
        id: String,
        text: String,
    },
    Run {
        task: String,
        project: Option<PathBuf>,
    },
}

/// A tm that mints `tm-1`, `tm-2`, … and reports states from a map.
#[derive(Default)]
pub(super) struct FakeTm {
    pub seen: Mutex<Vec<Seen>>,
    pub states: Mutex<HashMap<String, TmSessionState>>,
    /// Queued failures for the next `session_new` / `session_send`.
    pub fail_new: Mutex<VecDeque<CallError>>,
    pub fail_send: Mutex<VecDeque<CallError>>,
}

impl FakeTm {
    pub fn seen(&self) -> Vec<Seen> {
        self.seen.lock().expect("seen").clone()
    }

    pub fn set_state(&self, id: &str, state: TmSessionState) {
        self.states
            .lock()
            .expect("states")
            .insert(id.to_string(), state);
    }
}

#[async_trait]
impl TmClient for FakeTm {
    async fn session_new(&self, repo: &Path, task: &str) -> Result<String, CallError> {
        self.seen.lock().expect("seen").push(Seen::New {
            repo: repo.to_path_buf(),
            task: task.to_string(),
        });
        if let Some(e) = self.fail_new.lock().expect("fail").pop_front() {
            return Err(e);
        }
        let mut states = self.states.lock().expect("states");
        let id = format!("tm-{}", states.len() + 1);
        states.insert(id.clone(), TmSessionState::Live);
        Ok(id)
    }

    async fn session_state(&self, id: &str) -> Result<TmSessionState, CallError> {
        self.seen
            .lock()
            .expect("seen")
            .push(Seen::State { id: id.to_string() });
        Ok(self
            .states
            .lock()
            .expect("states")
            .get(id)
            .cloned()
            .unwrap_or(TmSessionState::Gone {
                reason: format!("session not found: {id}"),
            }))
    }

    async fn session_send(&self, id: &str, text: &str) -> Result<(), CallError> {
        self.seen.lock().expect("seen").push(Seen::Send {
            id: id.to_string(),
            text: text.to_string(),
        });
        match self.fail_send.lock().expect("fail").pop_front() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

/// A tc whose `task.run` answers `tc-<n>`, after an optional delay, or a
/// queued failure.
#[derive(Default)]
pub(super) struct FakeTc {
    pub seen: Mutex<Vec<Seen>>,
    pub fail: Mutex<VecDeque<CallError>>,
    pub delay: Option<Duration>,
}

impl FakeTc {
    pub fn seen(&self) -> Vec<Seen> {
        self.seen.lock().expect("seen").clone()
    }

    pub fn fail_next(&self, e: CallError) {
        self.fail.lock().expect("fail").push_back(e);
    }
}

#[async_trait]
impl TcClient for FakeTc {
    async fn task_run(&self, task: &str, project: Option<&Path>) -> Result<String, CallError> {
        let n = {
            let mut seen = self.seen.lock().expect("seen");
            seen.push(Seen::Run {
                task: task.to_string(),
                project: project.map(Path::to_path_buf),
            });
            seen.len()
        };
        if let Some(delay) = self.delay {
            tokio::time::sleep(delay).await;
        }
        match self.fail.lock().expect("fail").pop_front() {
            Some(e) => Err(e),
            None => Ok(format!("tc-{n}")),
        }
    }
}

/// A temp instance home `izzie` that exists on disk, with no config.
pub(super) fn home() -> (tempfile::TempDir, AssistantHome) {
    let root = tempfile::tempdir().expect("tempdir");
    let id = AssistantInstanceId::new("izzie").expect("id");
    let home = AssistantHome::under(root.path(), id);
    std::fs::create_dir_all(home.path()).expect("home dir");
    (root, home)
}
