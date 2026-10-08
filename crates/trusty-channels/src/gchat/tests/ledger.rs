//! The question ledger: restart survival and `[Q-<n>]` token parsing.

use wiremock::MockServer;

use super::{bootstrap, dm, mount_create, mount_token, Project, DM_JANET, JANET, THREE_ROUTES};
use crate::gchat::api::client::Endpoints;
use crate::gchat::inbound::InboundOutcome;
use crate::gchat::state::ledger::question_tokens;
use crate::gchat::{GchatChannel, StateError};

#[tokio::test]
async fn ledger_survives_restart() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    mount_create(&server).await;
    let project = Project::committed(THREE_ROUTES);
    {
        let first = project.channel(&server);
        bootstrap(&first, JANET, DM_JANET);
        first.send_question("janet", "One?").await.expect("q1");
        first.send_question("janet", "Two?").await.expect("q2");
        let report = first
            .process_batch(&[dm("a", JANET, DM_JANET, "[Q-1] yes")])
            .expect("batch");
        assert_eq!(
            report.outcomes,
            [InboundOutcome::Answered {
                question_id: 1,
                route: "janet".into()
            }]
        );
    }

    // A new instance over the same state dir.
    let second = project.channel(&server);
    assert!(
        second.question(1).expect("q1").answer.is_some(),
        "answer lost"
    );
    let q2 = second.question(2).expect("q2 survives");
    assert!(q2.is_open());
    assert_eq!(q2.route, "janet");
    assert!(q2.thread_name.is_some(), "thread name lost");
    assert_eq!(
        second.health().routes[0].space.as_deref(),
        Some(DM_JANET),
        "space lost"
    );

    let report = second
        .process_batch(&[dm("b", JANET, DM_JANET, "[Q-2] no")])
        .expect("batch");
    assert_eq!(
        report.outcomes,
        [InboundOutcome::Answered {
            question_id: 2,
            route: "janet".into()
        }]
    );
    let q3 = second.send_question("janet", "Three?").await.expect("q3");
    assert_eq!(q3.id, 3, "a restarted ledger reused an id");
}

#[test]
fn question_tokens_parse_only_well_formed_ids() {
    assert_eq!(question_tokens("re [Q-12]: yes"), [12]);
    assert_eq!(question_tokens("[Q-1] and [Q-2] and [Q-1]"), [1, 2]);
    assert!(question_tokens("[Q-] [Q-x] [q-1] Q-1 [Q-1 [Q-99999999999999999999999]").is_empty());
    assert_eq!(question_tokens("[Q-[Q-7]"), [7]);
}

fn ledger_path(project: &Project) -> std::path::PathBuf {
    project.dir().join(".trusty-channels/state/questions.jsonl")
}

fn seed_ledger(project: &Project, text: &str) {
    let dir = project.dir().join(".trusty-channels/state");
    std::fs::create_dir_all(&dir).expect("state dir");
    std::fs::write(ledger_path(project), text).expect("seed ledger");
}

fn open(project: &Project, server: &MockServer) -> Result<GchatChannel, StateError> {
    GchatChannel::open_with(project.dir(), Endpoints::single_host(&server.uri()))
}

#[tokio::test]
async fn second_open_on_one_state_dir_is_refused_until_the_first_drops() {
    let server = MockServer::start().await;
    let project = Project::committed(THREE_ROUTES);
    let first = project.channel(&server);
    let err = open(&project, &server).expect_err("two writers opened one state dir");
    assert!(matches!(err, StateError::Locked { .. }), "{err:?}");
    drop(first);
    open(&project, &server).expect("the lock is released on drop");
}

#[tokio::test]
async fn torn_final_ledger_line_is_quarantined_and_the_channel_opens() {
    let server = MockServer::start().await;
    let project = Project::committed(THREE_ROUTES);
    let whole = "{\"op\":\"reserve\",\"id\":5,\"ts\":\"2026-10-08T00:00:00Z\"}\n";
    let torn = "{\"op\":\"open\",\"id\":5,\"rou";
    seed_ledger(&project, &format!("{whole}{torn}"));
    let channel = open(&project, &server).expect("a torn final line must not fail the open");
    assert!(channel.question(5).is_none());
    assert_eq!(
        std::fs::read_to_string(ledger_path(&project)).expect("ledger"),
        whole,
        "the torn line was not cut"
    );
    let torn_file = project
        .dir()
        .join(".trusty-channels/state/questions.jsonl.torn");
    assert_eq!(
        std::fs::read_to_string(torn_file).expect("quarantine file"),
        format!("{torn}\n")
    );
    let lines = project.audit_lines();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["event"], "ledger_line_quarantined");
    assert_eq!(lines[0]["reason"], "torn_final_line");
    assert_eq!(lines[0]["length"], torn.len());
    drop(channel);

    // A whole record that lost only its newline is kept and terminated, so
    // the next append starts a new line.
    seed_ledger(&project, whole.trim_end());
    let channel = open(&project, &server).expect("open");
    drop(channel);
    assert_eq!(
        std::fs::read_to_string(ledger_path(&project)).expect("ledger"),
        whole
    );
    assert_eq!(
        project.audit_lines().len(),
        1,
        "a whole record was quarantined"
    );
}

#[tokio::test]
async fn malformed_ledger_line_before_the_last_still_fails_the_open() {
    let server = MockServer::start().await;
    let project = Project::committed(THREE_ROUTES);
    let reserve = |id: u64| format!("{{\"op\":\"reserve\",\"id\":{id},\"ts\":\"t\"}}\n");
    // A malformed middle line, and a malformed line followed by a torn one.
    for text in [
        format!("{}not json\n{}", reserve(1), reserve(2)),
        format!("{}not json\n{{\"op\":\"res", reserve(1)),
    ] {
        seed_ledger(&project, &text);
        let err = open(&project, &server).expect_err("a malformed middle line must fail");
        assert!(
            matches!(err, StateError::Corrupt { line: 2, .. }),
            "{err:?}"
        );
    }
}
