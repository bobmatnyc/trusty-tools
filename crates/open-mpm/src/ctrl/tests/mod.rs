//! Centralised CTRL tests.
//!
//! Why: The ctrl module is split across pm_task / ctrl_turn / repl / handlers
//! files but their unit tests share helpers (`insert_fake`, `insert_mock_actor`,
//! `make_fake_self_project`) and many cross-reference items from multiple
//! submodules. Co-locating the tests here lets every test reach the relevant
//! submodules via `super::super::*` without exposing implementation-detail
//! re-exports purely for test use.
//! What: Shared test helpers plus grouped `#[test]` / `#[tokio::test]`
//! submodules (config, state, commands, pm_task, tools) that previously lived
//! in a single `ctrl/tests.rs`.
//! Test: This module IS the test surface.

#![cfg(test)]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use tokio::sync::mpsc;

use super::state::{Ctrl, PmHandle, PmMsg};

mod command_tests;
mod config_tests;
mod pm_task_tests;
mod state_tests;
mod tools_tests;

/// Insert a fake PmHandle into ctrl for the given key/name.
pub(super) fn insert_fake(ctrl: &mut Ctrl, key: &str, name: &str) -> mpsc::Sender<PmMsg> {
    let (tx, _rx) = mpsc::channel(1);
    let tx2 = tx.clone();
    ctrl.pms.insert(
        key.to_string(),
        PmHandle {
            name: name.to_string(),
            project_path: PathBuf::from(key),
            tx,
            task: tokio::spawn(async {}),
            status: Arc::new(Mutex::new("idle".to_string())),
            last_message: Arc::new(Mutex::new(String::new())),
        },
    );
    tx2
}

/// Insert a mock actor that replies with `response` to the first Task.
pub(super) fn insert_mock_actor(ctrl: &mut Ctrl, key: &str, response: Result<String>) {
    let (tx, mut rx) = mpsc::channel::<PmMsg>(16);
    let task = tokio::spawn(async move {
        if let Some(PmMsg::Task { reply, .. }) = rx.recv().await {
            let _ = reply.send(response);
        }
    });
    ctrl.pms.insert(
        key.to_string(),
        PmHandle {
            name: key.to_string(),
            project_path: PathBuf::from(key),
            tx,
            task,
            status: Arc::new(Mutex::new("idle".to_string())),
            last_message: Arc::new(Mutex::new(String::new())),
        },
    );
    ctrl.active = Some(key.to_string());
}

/// Build a fake self-project layout under `tmp` and return the root.
pub(super) fn make_fake_self_project(tmp: &std::path::Path) -> PathBuf {
    let agents = tmp.join(".open-mpm").join("agents");
    std::fs::create_dir_all(&agents).unwrap();
    std::fs::write(agents.join("pm.toml"), "[agent]\nname=\"pm\"\n").unwrap();
    std::fs::write(
        tmp.join("Cargo.toml"),
        "[package]\nname = \"x\"\nversion = \"9.9.9\"\nedition = \"2021\"\n",
    )
    .unwrap();
    tmp.to_path_buf()
}
