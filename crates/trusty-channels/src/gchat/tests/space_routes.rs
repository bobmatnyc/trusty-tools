//! Space-configured routes (#9448 owner ruling): several routes share one
//! named Chat space; each sends there with no learned DM binding, and a reply
//! binds only from its route's recipient, in that configured space.

use std::path::Path;

use wiremock::MockServer;

use super::{api_requests, message, mount_create, mount_token, Project, ASKER, JANET};
use crate::gchat::api::client::Endpoints;
use crate::gchat::api::events::{ChatEvent, PulledMessage};
use crate::gchat::doctor::run_doctor;
use crate::gchat::error::RouteError;
use crate::gchat::inbound::InboundOutcome;
use crate::gchat::routes::parse_routes;

const BOB: &str = "bob@example.com";
const OTHER: &str = "rev@example.com";
const SHARED: &str = "spaces/SHARED";
const OTHER_SPACE: &str = "spaces/OTHER";

/// `bob` and `janet` share `spaces/SHARED`; `other` is configured on
/// `spaces/OTHER`; `asker` is a DM route with no `space`.
pub(super) const SPACE_ROUTES: &str = r#"version = 1

[gchat.connection]
project_id = "test-project"
subscription = "chat-in"
key_file = "{KEY}"

[[gchat.routes]]
name = "bob"
recipient = "bob@example.com"
kinds = ["question", "review_notice"]
space = "spaces/SHARED"

[[gchat.routes]]
name = "janet"
recipient = "janet@example.com"
kinds = ["question"]
space = "spaces/SHARED"

[[gchat.routes]]
name = "other"
recipient = "rev@example.com"
kinds = ["question"]
space = "spaces/OTHER"

[[gchat.routes]]
name = "asker"
recipient = "ask@example.com"
kinds = ["question"]
"#;

fn dropped(reason: &'static str) -> InboundOutcome {
    InboundOutcome::Dropped { reason }
}

/// A message in a named space (`spaceType` `SPACE`).
fn in_space(ack: &str, sender: &str, space: &str, text: &str) -> PulledMessage {
    message(ack, sender, space, "SPACE", text, None)
}

fn spaces_file(project: &Project) -> std::path::PathBuf {
    project
        .dir()
        .join(".trusty-channels/state/gchat-spaces.json")
}

/// `(reason, sender)` of every `inbound_dropped` audit line.
fn drops(project: &Project) -> Vec<(String, String)> {
    project
        .audit_lines()
        .into_iter()
        .filter(|l| l["event"] == "inbound_dropped")
        .map(|l| {
            (
                l["reason"].as_str().unwrap_or_default().to_string(),
                l["sender"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

#[test]
fn space_field_loads_and_a_malformed_space_fails_the_load() {
    let path = Path::new("routes.toml");
    let text = SPACE_ROUTES.replace("{KEY}", "/k.json");
    let table = parse_routes(path, &text, None).expect("space routes load");
    assert_eq!(table.routes.len(), 4, "two routes may share one space");

    for bad in [
        "rooms/SHARED",
        "SHARED",
        "spaces/",
        "spaces/A/messages",
        "spaces/A?x=1",
        "spaces/..",
        "spaces/a b",
    ] {
        let text = text.replacen("space = \"spaces/SHARED\"", &format!("space = {bad:?}"), 1);
        let err = parse_routes(path, &text, None).expect_err(bad);
        assert!(
            matches!(&err, RouteError::Invalid { entry, .. } if entry.contains("\"bob\"")),
            "{bad}: {err:?}"
        );
        assert!(err.to_string().contains("spaces/{space}"), "{bad}: {err}");
    }
}

#[tokio::test]
async fn space_route_sends_to_its_configured_space_without_a_learned_binding() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    mount_create(&server).await;
    let project = Project::committed(SPACE_ROUTES);
    let channel = project.channel(&server);

    channel.send_question("bob", "Ship it?").await.expect("bob");
    channel
        .send_review_notice("bob", "PR ready", "https://example.com/pr/1")
        .await
        .expect("notice");
    channel.send_question(JANET, "Merge?").await.expect("janet");
    let paths: Vec<String> = api_requests(&server)
        .await
        .iter()
        .map(|r| r.url.path().to_string())
        .collect();
    assert_eq!(paths, vec![format!("/v1/{SHARED}/messages"); 3]);
    assert!(
        !spaces_file(&project).exists(),
        "a space route wrote a binding"
    );
    assert_eq!(channel.health().routes[0].space.as_deref(), Some(SHARED));

    // The DM route beside it still learns its DM, and only its own.
    let report = channel
        .process_batch(&[message(
            "b",
            ASKER,
            "spaces/DMASK",
            "DIRECT_MESSAGE",
            "hi",
            None,
        )])
        .expect("batch");
    assert_eq!(report.outcomes, [dropped("space_learned")]);
    let learned = std::fs::read_to_string(spaces_file(&project)).expect("spaces file");
    assert!(
        learned.contains("spaces/DMASK") && !learned.contains(SHARED),
        "{learned}"
    );
}

#[tokio::test]
async fn two_routes_sharing_a_space_each_bind_their_own_question() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    mount_create(&server).await;
    let project = Project::committed(SPACE_ROUTES);
    let channel = project.channel(&server);
    let q1 = channel.send_question("bob", "First?").await.expect("q1");
    let q2 = channel.send_question("janet", "Second?").await.expect("q2");
    assert_eq!((q1.id, q2.id), (1, 2));

    let report = channel
        .process_batch(&[
            // Janet names Bob's question: it is not her route's question.
            in_space("c1", JANET, SHARED, "[Q-1] yes"),
            // Bob replies in his question's thread.
            message("c2", BOB, SHARED, "SPACE", "yes", q1.thread_name.as_deref()),
            // Janet replies by token, in a group chat typed payload.
            message("c3", JANET, SHARED, "GROUP_CHAT", "[Q-2] no", None),
        ])
        .expect("batch");
    assert_eq!(
        report.outcomes,
        [
            dropped("no_open_question"),
            InboundOutcome::Answered {
                question_id: 1,
                route: "bob".into()
            },
            InboundOutcome::Answered {
                question_id: 2,
                route: "janet".into()
            },
        ]
    );
    assert_eq!(
        channel.question(1).expect("q1").answer.expect("a").sender,
        BOB
    );
    assert_eq!(
        channel.question(2).expect("q2").answer.expect("a").sender,
        JANET
    );
    assert!(
        !spaces_file(&project).exists(),
        "a space route wrote a binding"
    );
}

/// 4a: a sender no route names, posting in the configured space.
#[tokio::test]
async fn unknown_sender_in_the_configured_space_is_dropped_and_audited() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    mount_create(&server).await;
    let project = Project::committed(SPACE_ROUTES);
    let channel = project.channel(&server);
    channel.send_question("bob", "Ship it?").await.expect("q1");

    let report = channel
        .process_batch(&[in_space("e1", "eve@example.com", SHARED, "[Q-1] yes")])
        .expect("batch");
    assert_eq!(report.outcomes, [dropped("sender_not_recipient")]);
    assert_eq!(report.ack_ids, ["e1"]);
    assert!(channel.question(1).expect("q1").is_open());
    assert_eq!(
        drops(&project),
        [("sender_not_recipient".into(), "eve@example.com".into())]
    );
}

/// 4b: a route's recipient posting in another route's configured space.
#[tokio::test]
async fn recipient_in_another_routes_space_is_dropped_and_audited() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    mount_create(&server).await;
    let project = Project::committed(SPACE_ROUTES);
    let channel = project.channel(&server);
    channel.send_question("bob", "Ship it?").await.expect("q1");

    let report = channel
        .process_batch(&[
            in_space("m1", BOB, OTHER_SPACE, "[Q-1] yes"),
            in_space("m2", OTHER, SHARED, "[Q-1] yes"),
        ])
        .expect("batch");
    assert_eq!(
        report.outcomes,
        [
            dropped("configured_space_mismatch"),
            dropped("configured_space_mismatch")
        ]
    );
    assert!(channel.question(1).expect("q1").is_open());
    assert_eq!(
        drops(&project),
        [
            ("configured_space_mismatch".into(), BOB.into()),
            ("configured_space_mismatch".into(), OTHER.into())
        ]
    );
}

/// 4c: a space route takes no DM, and a DM never binds it a space.
#[tokio::test]
async fn dm_from_a_space_route_recipient_is_dropped_and_audited() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    mount_create(&server).await;
    let project = Project::committed(SPACE_ROUTES);
    let channel = project.channel(&server);
    channel.send_question("bob", "Ship it?").await.expect("q1");

    let report = channel
        .process_batch(&[
            message("d1", BOB, "spaces/DMBOB", "DIRECT_MESSAGE", "hello", None),
            message(
                "d2",
                BOB,
                "spaces/DMBOB",
                "DIRECT_MESSAGE",
                "[Q-1] yes",
                None,
            ),
        ])
        .expect("batch");
    assert_eq!(
        report.outcomes,
        [
            dropped("direct_message_on_space_route"),
            dropped("direct_message_on_space_route")
        ]
    );
    assert!(channel.question(1).expect("q1").is_open());
    assert!(!spaces_file(&project).exists(), "a DM bound a space route");
    assert_eq!(channel.health().routes[0].space.as_deref(), Some(SHARED));
    assert_eq!(drops(&project).len(), 2);
}

/// 4d: a space no route configures, and a payload with no named-space type.
#[tokio::test]
async fn message_from_an_unconfigured_space_is_dropped_and_audited() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    mount_create(&server).await;
    let project = Project::committed(SPACE_ROUTES);
    let channel = project.channel(&server);
    channel.send_question("bob", "Ship it?").await.expect("q1");

    // The configured space's name, but no `spaceType`.
    let mut untyped = in_space("u2", BOB, SHARED, "[Q-1] yes");
    if let Ok(ChatEvent::Message(m)) = &mut untyped.event {
        m.space.space_type = None;
    }
    let report = channel
        .process_batch(&[in_space("u1", BOB, "spaces/RANDOM", "[Q-1] yes"), untyped])
        .expect("batch");
    assert_eq!(
        report.outcomes,
        [
            dropped("space_not_configured"),
            dropped("space_type_not_allowed")
        ]
    );
    assert!(channel.question(1).expect("q1").is_open());
    assert_eq!(
        drops(&project),
        [
            ("space_not_configured".into(), BOB.into()),
            ("space_type_not_allowed".into(), BOB.into())
        ]
    );
}

#[tokio::test]
async fn doctor_reports_a_space_route_as_configured() {
    let server = MockServer::start().await;
    let project = Project::committed(SPACE_ROUTES);
    let report = run_doctor(project.dir(), Endpoints::single_host(&server.uri()), true).await;
    let text = report.render();
    assert!(report.ok(), "{text}");
    let rows: Vec<&str> = text.lines().filter(|l| l.starts_with("route=")).collect();
    assert_eq!(rows.len(), 4, "{text}");
    for row in &rows[..3] {
        assert!(row.contains("space=configured (spaces/"), "{row}");
    }
    assert!(rows[3].contains("space=pending"), "{}", rows[3]);
}
