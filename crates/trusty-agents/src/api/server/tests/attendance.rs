//! `POST /api/task` attendance wiring (#4703).
//!
//! Why: `submit_task` is the GUI/WebUI's only task entry point, and its
//! attendance hook was the single most load-bearing one in the crate — yet it
//! was UNTESTABLE. It called the `$HOME`-resolving `note_turn`, so a test
//! written the obvious way ("submit a task, assert attendance was recorded")
//! wrote into the developer's real `~/.trusty-agents/attendance/ctrl.json` and
//! had nothing local to assert against. It therefore passed whether or not the
//! handler worked, which is exactly the near-miss PR #4695 caught in the REPL.
//! Now that `AppState` carries the root, the assertion is finally possible.
//! What: one test, driving the real router.
//! Test: this module IS the test.

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use tower::ServiceExt;

use super::super::routes::build_router;
use super::super::state::AppState;

/// A persona no roster can resolve. `submit_task` records attendance against
/// the requested agent, and the background future it spawns then fails on the
/// agent LOOKUP (a filesystem read) rather than reaching an LLM or a
/// subprocess — so this test stays hermetic and fast.
const PROBE_PERSONA: &str = "attendance-probe-4703";

/// The last human turn recorded for `persona` under `root`.
fn recorded_turn(root: &std::path::Path, persona: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    let tracker = crate::attendance::AttendanceTracker::new(
        root,
        crate::attendance::AttendanceConfig::default(),
    );
    let id = crate::assistants::AssistantInstanceId::new(persona).expect("valid id");
    tracker.last_human_turn(&id).expect("read")
}

/// #4703 regression: `POST /api/task` records attendance under the root
/// injected on `AppState`, not one resolved from `$HOME`.
///
/// Why: THE test the issue says was impossible to write. It is also the test
/// that proves the fix: deleting the `note_command_turn_in(...)` call from
/// `submit_task` must fail it — confirmed by doing exactly that.
/// What: points `AppState::attendance_root` at a tempdir, posts a real task
/// naming [`PROBE_PERSONA`], and asserts the record landed in the tempdir.
/// Nothing in the assertion path can be satisfied by a write to the real home.
#[tokio::test]
async fn submit_task_records_attendance_under_the_injected_root() {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let root = crate::attendance::attendance_root(dir.path());

    let state = AppState {
        attendance_root: Some(root.clone()),
        ..Default::default()
    };

    let app = build_router(state);
    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/task")
        .header("content-type", "application/json")
        .body(Body::from(format!(
            r#"{{"task":"hello","agent":"{PROBE_PERSONA}"}}"#
        )))
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::ACCEPTED,
        "submit_task should accept the task"
    );

    assert!(
        recorded_turn(&root, PROBE_PERSONA).is_some(),
        "submit_task must record the human turn under the INJECTED root; \
         finding nothing here means the hook resolved $HOME again (#4703)"
    );
}
