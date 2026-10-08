//! `gchat-mcp doctor` rows (#9448 D6): one per route, a broken key fails,
//! and a held state lock reads "in use" without failing.

use std::os::unix::fs::PermissionsExt;

use wiremock::MockServer;

use super::{bootstrap, mount_token_expect, Project, DM_JANET, JANET};
use crate::gchat::api::client::Endpoints;
use crate::gchat::doctor::{run_doctor, IN_USE};

/// Two routes: `janet` (both kinds) and `notices` (review notices only).
const TWO_ROUTES: &str = r#"version = 1

[gchat.connection]
project_id = "test-project"
subscription = "chat-in"
key_file = "{KEY}"

[[gchat.routes]]
name = "janet"
recipient = "janet@example.com"
kinds = ["question", "review_notice"]

[[gchat.routes]]
name = "notices"
recipient = "rev@example.com"
kinds = ["review_notice"]
"#;

fn endpoints(server: &MockServer) -> Endpoints {
    Endpoints::single_host(&server.uri())
}

#[tokio::test]
async fn doctor_prints_one_row_per_route_and_passes() {
    let server = MockServer::start().await;
    let project = Project::committed(TWO_ROUTES);
    {
        let channel = project.channel(&server);
        bootstrap(&channel, JANET, DM_JANET);
    }

    let report = run_doctor(project.dir(), endpoints(&server), true).await;
    let text = report.render();
    assert!(report.ok(), "{text}");
    assert_eq!(report.exit_code(), 0);
    let rows: Vec<&str> = text.lines().filter(|l| l.starts_with("route=")).collect();
    assert_eq!(rows.len(), 2, "{text}");
    assert!(
        rows[0].starts_with(
            "route=janet recipient=janet@example.com kinds=question,review_notice gate=ok"
        ),
        "{}",
        rows[0]
    );
    assert!(rows[0].contains("key_file=ok"), "{}", rows[0]);
    assert!(rows[0].contains("token=skipped (--offline)"), "{}", rows[0]);
    assert!(rows[0].contains("space=ok (bound)"), "{}", rows[0]);
    assert!(rows[0].contains("open_questions=ok (0)"), "{}", rows[0]);
    assert!(
        rows[0].ends_with("subscription=projects/test-project/subscriptions/chat-in"),
        "{}",
        rows[0]
    );
    assert!(
        rows[1].starts_with("route=notices recipient=rev@example.com kinds=review_notice"),
        "{}",
        rows[1]
    );
    assert!(
        rows[1].contains("space=pending"),
        "an unbound space is not a failure: {}",
        rows[1]
    );
    assert!(text.ends_with("result: ok\n"), "{text}");
    assert!(
        server
            .received_requests()
            .await
            .expect("recording")
            .is_empty(),
        "offline"
    );
}

#[tokio::test]
async fn doctor_fails_on_a_broken_key_mode() {
    let server = MockServer::start().await;
    let project = Project::committed(TWO_ROUTES);
    let key = project.key_file();
    std::fs::set_permissions(key, std::fs::Permissions::from_mode(0o644)).expect("chmod");

    let report = run_doctor(project.dir(), endpoints(&server), true).await;
    let text = report.render();
    assert!(!report.ok(), "{text}");
    assert_eq!(report.exit_code(), 1);
    assert!(report.rows.iter().all(|r| r.key_file.is_failed()), "{text}");
    assert!(text.contains("mode 644"), "{text}");
    assert!(text.ends_with("result: FAILED\n"), "{text}");
}

#[tokio::test]
async fn doctor_mints_a_token_online_and_reports_in_use_state() {
    let server = MockServer::start().await;
    mount_token_expect(&server, 3600, 1).await;
    let project = Project::committed(TWO_ROUTES);
    let _held = project.channel(&server);

    let report = run_doctor(project.dir(), endpoints(&server), false).await;
    let text = report.render();
    assert!(report.ok(), "in use is not a failure: {text}");
    assert_eq!(report.exit_code(), 0);
    assert_eq!(report.rows.len(), 2, "{text}");
    for row in &report.rows {
        assert_eq!(row.space.detail, IN_USE, "{text}");
        assert_eq!(row.open_questions.detail, IN_USE, "{text}");
        assert_eq!(row.token.status, "ok", "{text}");
        assert_eq!(row.gate.status, "ok", "{text}");
    }
    assert!(text.contains("in use by a running gchat-mcp"), "{text}");
}

#[tokio::test]
async fn doctor_fails_without_a_routes_file() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().expect("dir");
    let report = run_doctor(dir.path(), endpoints(&server), true).await;
    assert!(!report.ok());
    assert!(
        report.render().contains("load: failed (no routes file"),
        "{}",
        report.render()
    );
}
