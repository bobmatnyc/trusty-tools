//! #9139 regression: a spawned `tcode` must never reach the memory daemon, data
//! root, `HOME` or palace named by the environment the test suite runs in.
//!
//! Why: the e2e helpers spawned `tcode` with the parent's whole environment. In
//! a trusty-mpm managed shell that names the live palace
//! (`TRUSTY_MEMORY_PALACE=trusty-tools`) and resolves the live daemon's socket,
//! so every turn-recording test wrote its fixture turns into the live palace:
//! 2,308 `turn` drawers. Isolation that depends on the caller's environment
//! fails whenever that environment is hostile; this test makes it hostile on
//! purpose.
//! What: the one test in this binary points `TRUSTY_MEMORY_SOCKET` at a
//! SENTINEL mock daemon that records every call, and points
//! `TRUSTY_DATA_DIR_OVERRIDE` and `HOME` at sentinel directories. It then
//! drives a turn-recording run over `tcode serve --stdio` and over
//! `tcode run-task`, and asserts the sentinel daemon saw no call and both
//! sentinel directories are still empty. The sentinel is a mock, so even a
//! regressed run writes nothing real.
//! Test: this file IS the test.

mod support;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use support::{StdioSession, project_with_agents, run_task_to_completion};

/// How long a regressed child gets to dial the sentinel after its run ends —
/// the turn recorder drains on a detached task.
const DRAIN_WINDOW: Duration = Duration::from_secs(5);

/// The palace a trusty-mpm managed shell names for this repository.
const AMBIENT_PALACE: &str = "trusty-tools";

#[test]
fn turn_recording_children_never_reach_the_ambient_memory_daemon() {
    let sentinel_root = tempfile::tempdir().expect("sentinel root");
    let sentinel_data = sentinel_root.path().join("data");
    let sentinel_home = sentinel_root.path().join("home");
    std::fs::create_dir(&sentinel_data).expect("sentinel data root");
    std::fs::create_dir(&sentinel_home).expect("sentinel home");

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("tokio runtime");
    let calls: Arc<Mutex<Vec<String>>> = Arc::default();
    let recorded = Arc::clone(&calls);
    let sentinel = runtime.block_on(support::spawn_mock_memory_daemon(
        move |method: &str, params: Value| -> support::uds_mock::MockFuture {
            let tool = params["name"].as_str().unwrap_or_default();
            recorded
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(format!("{method} {tool}"));
            let answer = support::uds_mock::tools_call_envelope(&json!({"ok": true}));
            Box::pin(async move { Ok(answer) })
        },
    ));

    // SAFETY: this binary holds this one test, and no thread reads the
    // environment while it is written — the runtime's workers are idle and
    // no child has been spawned yet.
    unsafe {
        std::env::set_var(
            trusty_common::memory_rpc::TRUSTY_MEMORY_SOCKET_ENV,
            sentinel.socket(),
        );
        std::env::set_var(
            trusty_common::data_dir::DATA_DIR_OVERRIDE_ENV,
            &sentinel_data,
        );
        std::env::set_var("HOME", &sentinel_home);
        std::env::set_var(trusty_common::PALACE_OVERRIDE_ENV, AMBIENT_PALACE);
    }

    let project = project_with_agents();
    runtime.block_on(async {
        let mut daemon = StdioSession::spawn_with_mock_llm(project.path());
        run_task_to_completion(&mut daemon, "say hi").await;

        let cli = support::tcode_command()
            .args(["run-task", "pm", "say hi", "--project"])
            .arg(project.path())
            .env(trusty_code::task::mock_llm::MOCK_LLM_ENV, "echo")
            .output()
            .expect("spawn tcode run-task");
        assert!(
            cli.status.success(),
            "tcode run-task failed: {}",
            String::from_utf8_lossy(&cli.stderr)
        );

        // Keep the stdio daemon alive through the window so a regressed drain
        // task has every chance to dial the sentinel.
        let deadline = tokio::time::Instant::now() + DRAIN_WINDOW;
        while tokio::time::Instant::now() < deadline && seen(&calls).is_empty() {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        drop(daemon);
    });

    assert!(
        seen(&calls).is_empty(),
        "a spawned tcode dialled the ambient memory daemon — its turns would land \
         in the live `{AMBIENT_PALACE}` palace (#9139): {:?}",
        seen(&calls)
    );
    for (label, dir) in [("data root", &sentinel_data), ("HOME", &sentinel_home)] {
        let written: Vec<_> = std::fs::read_dir(dir)
            .expect("read sentinel dir")
            .map(|entry| entry.expect("sentinel entry").path())
            .collect();
        assert!(
            written.is_empty(),
            "a spawned tcode wrote into the ambient {label} (#9139): {written:?}"
        );
    }
}

/// Snapshot of the calls the sentinel daemon has answered so far.
fn seen(calls: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
    calls.lock().unwrap_or_else(|e| e.into_inner()).clone()
}
