//! Tests for the daemon's inbound answer body (#9873, #8433 P5).
//!
//! Why: the glue joins the decision and the delivery. Each test goes red
//! against a glue that delivers a question twice, reports an aborted
//! decision as fine, or reaches anything but the asker's pane.
//! What: `run_inbound` over the decision's counting fake (a real table) and
//! the deliverer's recorder; source scans of the inbound path.
//! Test: this is the test module.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::http::StatusCode;

use super::{InboundReply, reply, run_inbound};
use crate::channels::deliver::tests::Recorder;
use crate::channels::inbound::InboundEnv;
use crate::channels::inbound::tests::{Fake, OWNER, TableMode, ev, owner_conv};

/// The fake as the glue takes it.
fn shared(fake: Fake) -> (Arc<Fake>, Arc<dyn InboundEnv + Send + Sync>) {
    let fake = Arc::new(fake);
    let env: Arc<dyn InboundEnv + Send + Sync> = Arc::clone(&fake) as _;
    (fake, env)
}

/// 🔴 #9873 (AC3, AC5): two answers racing on one question resolve it once
/// and deliver once; the loser is a 409 naming the winner. 200 rounds.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_inbound_answers_deliver_once() {
    for round in 0..200 {
        let mut fake = Fake::new();
        fake.gate = Some(Arc::new(std::sync::Barrier::new(2)));
        let id = fake.open("owner-0", OWNER, &owner_conv());
        let (fake, env) = shared(fake);
        let rec = Recorder::new();
        let (a, b) = tokio::join!(
            run_inbound(Arc::clone(&env), &rec, ev(OWNER, &owner_conv(), "approve")),
            run_inbound(Arc::clone(&env), &rec, ev(OWNER, &owner_conv(), "reject")),
        );
        let replies = [a, b];
        let delivered = replies
            .iter()
            .filter(|r| matches!(r, InboundReply::Delivered { question_id } if *question_id == id))
            .count();
        assert_eq!(delivered, 1, "round {round}: {replies:?}");
        let lost = replies
            .iter()
            .find(|r| !matches!(r, InboundReply::Delivered { .. }))
            .expect("one loser");
        let (status, body) = reply(lost);
        assert_eq!(status, StatusCode::CONFLICT, "round {round}: {body:?}");
        let first = fake.table.answer(id).expect("the winner");
        assert_eq!(body.0["already_answered_by"], first.answerer);
        assert_eq!(body.0["at"], first.answered_at);
        assert_eq!(rec.sent().len(), 1, "round {round}: one send");
        assert_eq!(fake.resolves(), 2, "round {round}: both reached the CAS");
    }
}

/// 🔴 #9873 (F16): a decision task that dies after `resolve` is a 500
/// `inbound_aborted`, never ok; nothing is sent, and the doctor row names
/// the resolved question that has no delivery record.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn aborted_task_after_resolve_is_reported_not_ok() {
    let mut fake = Fake::new();
    fake.table_mode = TableMode::PanicsAfterResolve;
    let id = fake.open("owner-0", OWNER, &owner_conv());
    let (fake, env) = shared(fake);
    let rec = Recorder::new();
    let outcome = run_inbound(env, &rec, ev(OWNER, &owner_conv(), "approve")).await;
    assert!(matches!(outcome, InboundReply::Aborted(_)), "{outcome:?}");
    let (status, body) = reply(&outcome);
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body.0["error"], "inbound_aborted");
    assert!(rec.sent().is_empty(), "nothing was sent");
    assert!(fake.table.state(id).expect("known").answered, "resolved");
    let taint = crate::core::channels_taint::taint_dir(&fake.home);
    let row = crate::daemon::doctor::doctor_channel_taint::channel_taint_row(
        &taint,
        &std::collections::HashSet::new(),
    );
    assert_eq!(
        row.status,
        crate::core::doctor::CheckStatus::Fail,
        "{}",
        row.message
    );
    assert!(
        row.message.contains(&format!("Q-{id} to asker ")),
        "{}",
        row.message
    );
}

/// The crate's `src` directory.
fn src() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// `text` without its `//` comment lines.
fn code_lines(text: &str) -> String {
    text.lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The calls the inbound path must never make: a process, a tool, a hook,
/// or a session lifecycle step other than `inject`.
const FORBIDDEN: &[&str] = &[
    "Command",
    "process::",
    "tokio::spawn(",
    ".spawn(",
    "launch",
    "hook",
    "tool_use",
    ".create(",
    ".resume(",
    ".start(",
    ".restart(",
    "spawn_managed",
    "resume_managed",
    "send_input",
    "answer_decision",
];

/// The forbidden calls in `source`.
fn tool_calls(source: &str) -> Vec<&'static str> {
    let code = code_lines(source);
    FORBIDDEN
        .iter()
        .copied()
        .filter(|f| code.contains(f))
        .collect()
}

/// 🔴 #9873 (r5 §3): the inbound path runs no tool, process, hook or
/// session start; the only session call it names is `inject`.
#[test]
fn inbound_never_runs_a_tool() {
    let files = [
        "channels/inbound.rs",
        "channels/deliver.rs",
        "daemon/managed_routes/inbound.rs",
        "daemon/managed_routes/inbound_fake.rs",
    ];
    for file in files {
        let text = std::fs::read_to_string(src().join(file)).expect("source");
        assert_eq!(tool_calls(&text), Vec::<&str>::new(), "{file}");
    }
    let glue = code_lines(
        &std::fs::read_to_string(src().join("daemon/managed_routes/inbound.rs")).expect("glue"),
    );
    assert_eq!(glue.matches(".inject(").count(), 1, "one pane write");
    // The scan can fail: a path that started a process is caught.
    let bad = "fn f() { std::process::Command::new(\"sh\").spawn(); }";
    assert_eq!(tool_calls(bad), ["Command", "process::", ".spawn("]);
}

/// The names that mean a file can reach the question table.
const TABLE_NAMES: &[&str] = &[
    "QuestionTable",
    "question_table",
    "AnswerTable",
    "ResolveOutcome",
];

/// Whether `source` both names the question table and calls a `resolve`.
fn resolves_a_question(source: &str) -> bool {
    let code = code_lines(source);
    TABLE_NAMES.iter().any(|n| code.contains(n))
        && (code.contains(".resolve(") || code.contains("::resolve("))
}

/// Every production `.rs` file under `dir` (test files and `tests/` skipped).
fn production_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read src").flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            if name != "tests" {
                production_sources(&path, out);
            }
        } else if name.ends_with(".rs")
            && !name.ends_with("tests.rs")
            && !name.ends_with("_test.rs")
        {
            out.push(path);
        }
    }
}

/// 🔴 #9873 (Q8, AC5): `QuestionTable::resolve` is called only from
/// `channels/inbound.rs`, whose `Resolved` goes only to the deliverer.
#[test]
fn resolve_is_called_only_from_inbound() {
    let root = src();
    let mut files = Vec::new();
    production_sources(&root, &mut files);
    assert!(
        files.len() > 100,
        "the walk found the crate: {}",
        files.len()
    );
    let allowed = [
        root.join("channels/inbound.rs"),
        root.join("channels/question_table.rs"),
    ];
    let callers: Vec<_> = files
        .iter()
        .filter(|f| !allowed.contains(f))
        .filter(|f| resolves_a_question(&std::fs::read_to_string(f).unwrap_or_default()))
        .collect();
    assert_eq!(callers, Vec::<&PathBuf>::new());
    // The scan can fail: a second caller is caught, an unrelated `resolve` is not.
    let second = "use crate::channels::question_table::QuestionTable;\n\
                  fn f(t: &QuestionTable) { t.resolve(1, \"U1\", &c, \"yes\"); }";
    assert!(resolves_a_question(second));
    assert!(!resolves_a_question(
        "fn g() { ManifestSources::resolve(a, b); }"
    ));
}

/// The deliverer's real env over the shared fake driver, holding one `Active`
/// record bound to pane `%7` (#10127).
pub(super) async fn deliver_env_on_pane(
    dir: &tempfile::TempDir,
) -> (
    super::DaemonDeliverEnv,
    Arc<crate::session_manager::tests::FakeTmuxDriver>,
    String,
) {
    let fake = crate::session_manager::tests::FakeTmuxDriver::new();
    *fake.pane_id_override.lock().expect("pane id") = Some("%7".into());
    let mgr = crate::session_manager::SessionManager::new(dir.path(), fake.clone())
        .await
        .expect("manager");
    let ws = dir.path().to_path_buf();
    let record = mgr
        .create(
            "task".into(),
            Some(ws.clone()),
            Some("inbound-harness".into()),
            Some(ws.clone()),
            None,
            None,
        )
        .await
        .expect("create");
    mgr.set_workspace(
        &record.id,
        ws,
        crate::session_manager::ManagedSessionState::Active,
    )
    .await
    .expect("active");
    let env = super::DaemonDeliverEnv {
        mgr: Arc::new(mgr),
        dir: Some(dir.path().join("taint")),
    };
    (env, fake, record.id.to_string())
}

/// Every key the fake recorded, on any send primitive.
pub(super) fn keys_typed(fake: &crate::session_manager::tests::FakeTmuxDriver) -> usize {
    fake.pane_send_calls.lock().expect("sends").len()
        + fake.pane_literal_calls.lock().expect("literals").len()
        + fake.send_calls.lock().expect("session sends").len()
}

/// 🔴 #10127 (df-Da4): the inbound deliverer's send into a pane whose harness
/// has exited, or cannot be confirmed, is refused with a stable code and
/// types nothing — the line never reaches a shell prompt.
#[tokio::test]
async fn inbound_send_into_an_exited_harness_pane_types_nothing() {
    use crate::channels::deliver::DeliverEnv;
    use crate::core::process::PaneClaude;
    for (verdict, code) in [
        (PaneClaude::Absent, "no_live_harness"),
        (PaneClaude::Unknown, "harness_unconfirmed"),
    ] {
        let dir = crate::test_support::hermetic_temp_dir();
        let (env, fake, id) = deliver_env_on_pane(&dir).await;
        *fake.pane_claude_override.lock().expect("probe") = Some(verdict);
        let line = crate::channels::deliver::frame_answer(
            7,
            trusty_channels::policy::Channel::Slack,
            OWNER,
            "$(touch /tmp/x)",
        );
        let err = env.send(&id, &line).await.expect_err("refused");
        assert!(err.contains(code), "{verdict:?}: {err}");
        assert_eq!(keys_typed(&fake), 0, "{verdict:?}: nothing typed");
    }
}

/// #10127 positive control: a pane running its harness takes the line, on
/// the record's own pane.
#[tokio::test]
async fn inbound_send_into_a_live_harness_pane_is_typed() {
    use crate::channels::deliver::DeliverEnv;
    let dir = crate::test_support::hermetic_temp_dir();
    let (env, fake, id) = deliver_env_on_pane(&dir).await;
    *fake.pane_claude_override.lock().expect("probe") =
        Some(crate::core::process::PaneClaude::Present);
    env.send(&id, "the line").await.expect("sent");
    let sends = fake.pane_send_calls.lock().expect("sends").clone();
    assert_eq!(sends.len(), 1, "{sends:?}");
    assert_eq!(
        (sends[0].1.as_str(), sends[0].2.as_str()),
        ("%7", "the line")
    );
}
