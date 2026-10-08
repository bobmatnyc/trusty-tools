//! The egress gate: route, kind, URL and space checks run before any token
//! mint or Chat request.

use std::os::unix::fs::PermissionsExt;

use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

use super::{
    all_requests, api_requests, bootstrap, dm, mount_create, mount_token, Project, ASKER, DM_JANET,
    JANET, REVIEWER, THREE_ROUTES,
};
use crate::gchat::channel::{LoadStatus, RouteHealth};
use crate::gchat::error::SendError;
use crate::gchat::routes::MessageKind;

const SENTINEL: &str = "SENTINEL-7f3a-do-not-log";

#[tokio::test]
async fn unknown_recipient_is_refused_with_no_request_and_an_audit_line() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    mount_create(&server).await;
    let project = Project::committed(THREE_ROUTES);
    let channel = project.channel(&server);
    bootstrap(&channel, JANET, DM_JANET);

    for to in ["stranger@example.com", "nobody", "JANET@example.org"] {
        let err = channel.send_question(to, "Ship it?").await.expect_err(to);
        assert!(matches!(err, SendError::NoRoute { .. }), "{to}: {err:?}");
        let err = channel
            .send_review_notice(to, "Review", "https://example.com/pr/1")
            .await
            .expect_err(to);
        assert!(matches!(err, SendError::NoRoute { .. }), "{to}: {err:?}");
    }
    assert_eq!(
        all_requests(&server).await,
        0,
        "a refused send reached the server"
    );
    let refused: Vec<_> = project
        .audit_lines()
        .into_iter()
        .filter(|l| l["event"] == "send_refused")
        .collect();
    assert_eq!(refused.len(), 6);
    assert!(refused.iter().all(|l| l["reason"] == "no_route"));
    assert_eq!(refused[0]["to"], "stranger@example.com");
}

#[tokio::test]
async fn kind_not_allowed_is_refused_with_no_request() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    mount_create(&server).await;
    let project = Project::committed(THREE_ROUTES);
    let channel = project.channel(&server);
    bootstrap(&channel, REVIEWER, "spaces/DMREV");
    bootstrap(&channel, ASKER, "spaces/DMASK");

    let err = channel
        .send_question("notices", "Q?")
        .await
        .expect_err("question");
    assert!(
        matches!(&err, SendError::KindNotAllowed { route, kind: "question" } if route == "notices"),
        "{err:?}"
    );
    let err = channel
        .send_review_notice(ASKER, "Review", "https://example.com/t/1")
        .await
        .expect_err("review notice");
    assert!(matches!(err, SendError::KindNotAllowed { .. }), "{err:?}");
    assert_eq!(all_requests(&server).await, 0);
    let reasons: Vec<_> = project
        .audit_lines()
        .into_iter()
        .map(|l| l["reason"].clone())
        .collect();
    assert!(
        reasons.iter().filter(|r| *r == "kind_not_allowed").count() == 2,
        "{reasons:?}"
    );
}

#[tokio::test]
async fn review_notice_without_https_url_is_refused() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    mount_create(&server).await;
    let project = Project::committed(THREE_ROUTES);
    let channel = project.channel(&server);
    bootstrap(&channel, JANET, DM_JANET);
    for url in [
        "",
        "http://example.com/pr/1",
        "ftp://example.com/x",
        "example.com/pr/1",
        "https://",
        "javascript:alert(1)",
        "https://exa mple.com/x",
        "file:///etc/passwd",
    ] {
        let err = channel
            .send_review_notice("janet", "Review please", url)
            .await
            .expect_err(url);
        assert!(
            matches!(err, SendError::InvalidReviewUrl { .. }),
            "{url:?}: {err:?}"
        );
    }
    assert_eq!(all_requests(&server).await, 0);
}

#[tokio::test]
async fn valid_review_notice_is_sent_and_opens_no_question() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    mount_create(&server).await;
    let project = Project::committed(THREE_ROUTES);
    let channel = project.channel(&server);
    bootstrap(&channel, JANET, DM_JANET);
    let ledger = project.dir().join(".trusty-channels/state/questions.jsonl");

    let sent = channel
        .send_review_notice(
            "janet",
            "PR #12 waits for your review",
            "https://git.example.org/pr/12",
        )
        .await
        .expect("valid notice");
    assert_eq!(sent.route, "janet");
    let requests = api_requests(&server).await;
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].url.path(), format!("/v1/{DM_JANET}/messages"));
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).expect("json");
    assert_eq!(
        body["text"],
        "PR #12 waits for your review\nhttps://git.example.org/pr/12"
    );
    assert!(
        body.get("thread").is_none(),
        "a notice carries no thread key"
    );
    assert!(
        !ledger.exists(),
        "a review notice touched the question ledger"
    );
    assert!(channel.question(1).is_none());
}

#[tokio::test]
async fn unlearned_space_is_refused_without_fallback() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    mount_create(&server).await;
    let project = Project::committed(THREE_ROUTES);
    let channel = project.channel(&server);
    bootstrap(&channel, JANET, DM_JANET);

    let err = channel
        .send_question("asker", "Q?")
        .await
        .expect_err("no space");
    assert!(
        matches!(&err, SendError::SpaceNotLearned { route } if route == "asker"),
        "{err:?}"
    );
    assert!(
        err.to_string().contains("must message the Chat app first"),
        "{err}"
    );
    assert_eq!(all_requests(&server).await, 0, "fell back to another space");
    assert!(project.audit_text().contains("space_not_learned"));
}

#[tokio::test]
async fn egress_per_route_allows_exactly_its_kinds() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    mount_create(&server).await;
    let project = Project::committed(THREE_ROUTES);
    let channel = project.channel(&server);
    let spaces = [
        ("janet", JANET, DM_JANET),
        ("notices", REVIEWER, "spaces/DMREV"),
        ("asker", ASKER, "spaces/DMASK"),
    ];
    for (_, email, space) in spaces {
        bootstrap(&channel, email, space);
    }
    let table = channel.routes.as_ref().expect("loaded").clone();
    assert!(table.routes.len() >= 2);
    let mut expected_requests = 0;
    for route in &table.routes {
        let space = spaces.iter().find(|s| s.0 == route.name).expect("space").2;
        for kind in MessageKind::ALL {
            // Both address forms: route name and recipient email.
            for to in [route.name.as_str(), route.recipient.as_str()] {
                let before = api_requests(&server).await.len();
                let result = match kind {
                    MessageKind::Question => channel.send_question(to, "Q?").await.map(|_| ()),
                    MessageKind::ReviewNotice => channel
                        .send_review_notice(to, "Review", "https://example.com/t/1")
                        .await
                        .map(|_| ()),
                };
                let after = api_requests(&server).await;
                if route.allows(kind) {
                    result.unwrap_or_else(|e| panic!("{} {kind:?} via {to}: {e}", route.name));
                    expected_requests += 1;
                    assert_eq!(after.len(), before + 1);
                    assert_eq!(
                        after.last().map(|r| r.url.path().to_string()),
                        Some(format!("/v1/{space}/messages")),
                        "{} sent to the wrong space",
                        route.name
                    );
                } else {
                    assert!(
                        matches!(result, Err(SendError::KindNotAllowed { .. })),
                        "{} {kind:?} via {to} not refused",
                        route.name
                    );
                    assert_eq!(
                        after.len(),
                        before,
                        "{} {kind:?} sent a request",
                        route.name
                    );
                }
                assert_eq!(
                    channel.check_egress(kind, to).is_ok(),
                    route.allows(kind),
                    "check_egress disagrees for {} {kind:?}",
                    route.name
                );
            }
        }
    }
    for kind in MessageKind::ALL {
        assert!(matches!(
            channel.check_egress(kind, "outsider@example.com"),
            Err(SendError::NoRoute { .. })
        ));
    }
    assert_eq!(api_requests(&server).await.len(), expected_requests);
}

#[tokio::test]
async fn audit_lines_never_contain_message_text() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    mount_create(&server).await;
    let project = Project::committed(THREE_ROUTES);
    let channel = project.channel(&server);
    bootstrap(&channel, JANET, DM_JANET);
    let text = format!("please read {SENTINEL} now");

    let _ = channel.send_question("stranger@example.com", &text).await;
    let _ = channel.send_question("notices", &text).await;
    let _ = channel
        .send_review_notice("janet", &text, "http://x.example.com")
        .await;
    let report = channel
        .process_batch(&[
            dm("a1", JANET, DM_JANET, &text),
            dm("a2", "eve@example.com", DM_JANET, &text),
        ])
        .expect("batch");
    assert_eq!(report.ack_ids.len(), 2);

    let audit = project.audit_text();
    // The bootstrap DM, three refused sends, two dropped replies.
    assert_eq!(audit.lines().count(), 6, "{audit}");
    assert!(
        !audit.contains(SENTINEL),
        "message text leaked into the audit log:\n{audit}"
    );
    assert!(!audit.contains("please read"), "{audit}");
    for line in project
        .audit_lines()
        .iter()
        .filter(|l| l["message_id"] != "id-boot")
    {
        assert_eq!(line["length"], text.len(), "{line}");
    }
}

#[tokio::test]
async fn health_reports_each_route_and_the_load_status() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    mount_create(&server).await;
    let project = Project::committed(THREE_ROUTES);
    let channel = project.channel(&server);
    bootstrap(&channel, JANET, DM_JANET);
    channel.send_question("janet", "Q?").await.expect("sent");

    let health = channel.health();
    assert_eq!(health.load, LoadStatus::Loaded { routes: 3 });
    assert_eq!(health.client, Ok(()));
    assert_eq!(health.routes.len(), 3);
    assert_eq!(
        health.routes[0],
        RouteHealth {
            name: "janet".into(),
            recipient: JANET.into(),
            kinds: vec!["question", "review_notice"],
            space: Some(DM_JANET.into()),
            open_questions: 1,
        }
    );
    assert_eq!(health.routes[1].space, None);
    assert_eq!(health.routes[2].open_questions, 0);
}

#[tokio::test]
async fn post_then_ledger_write_failure_reports_sent_not_recorded() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    let project = Project::committed(THREE_ROUTES);
    let channel = project.channel(&server);
    bootstrap(&channel, JANET, DM_JANET);
    let ledger = project.dir().join(".trusty-channels/state/questions.jsonl");
    // The Chat post succeeds, and the ledger turns read-only while it runs:
    // the reserve line is on disk and the open line cannot be written.
    let read_only = ledger.clone();
    Mock::given(method("POST"))
        .and(path(format!("/v1/{DM_JANET}/messages")))
        .respond_with(move |_: &Request| {
            std::fs::set_permissions(&read_only, std::fs::Permissions::from_mode(0o444))
                .expect("chmod ledger");
            ResponseTemplate::new(200).set_body_json(json!({
                "name": format!("{DM_JANET}/messages/M1"),
                "thread": {"name": format!("{DM_JANET}/threads/T1")}
            }))
        })
        .expect(1)
        .mount(&server)
        .await;

    let err = channel
        .send_question("janet", "Ship it?")
        .await
        .expect_err("the open record could not be written");
    match &err {
        SendError::SentNotRecorded {
            id, message_name, ..
        } => {
            assert_eq!(*id, 1);
            assert_eq!(message_name, &format!("{DM_JANET}/messages/M1"));
        }
        other => panic!("a posted question reads as not sent: {other:?}"),
    }
    assert!(err.to_string().contains("do not resend"), "{err}");
    assert!(channel.question(1).is_none());
}
