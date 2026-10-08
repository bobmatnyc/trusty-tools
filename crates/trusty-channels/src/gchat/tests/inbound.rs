//! Inbound binding: space bootstrap, reply-to-question binding by thread
//! name and by `[Q-<n>]` token, and the drop-and-audit paths.

use std::os::unix::fs::PermissionsExt;

use serde_json::json;
use wiremock::matchers::{body_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::{
    api_requests, bootstrap, dm, message, mount_create, mount_token, pulled, Project, ASKER,
    DM_JANET, JANET, SUBSCRIPTION, THREE_ROUTES,
};
use crate::gchat::api::error::EventParseError;
use crate::gchat::api::events::ChatEvent;
use crate::gchat::error::{InboundError, SendError};
use crate::gchat::inbound::InboundOutcome;

fn dropped(reason: &'static str) -> InboundOutcome {
    InboundOutcome::Dropped { reason }
}

#[tokio::test]
async fn bootstrap_binds_only_recipient_dm() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    mount_create(&server).await;
    let project = Project::committed(THREE_ROUTES);
    let channel = project.channel(&server);

    // The recipient, but in a named space and a group chat: no binding.
    let report = channel
        .process_batch(&[
            message("s1", JANET, "spaces/ROOM", "SPACE", "hi", None),
            message("s2", JANET, "spaces/GROUP", "GROUP_CHAT", "hi", None),
            // A DM from someone no route names: no binding either.
            dm("s3", "eve@example.com", "spaces/DMEVE", "hi"),
        ])
        .expect("batch");
    assert_eq!(
        report.outcomes,
        [
            dropped("not_direct_message"),
            dropped("not_direct_message"),
            dropped("sender_not_recipient")
        ]
    );
    let err = channel
        .send_question("janet", "Q?")
        .await
        .expect_err("unbound");
    assert!(matches!(err, SendError::SpaceNotLearned { .. }), "{err:?}");
    assert_eq!(channel.health().routes[0].space, None);

    // The recipient in a DM binds the route.
    let report = channel
        .process_batch(&[dm("s4", "Janet@Example.com", DM_JANET, "hello")])
        .expect("batch");
    assert_eq!(report.outcomes, [dropped("space_learned")]);
    assert_eq!(channel.health().routes[0].space.as_deref(), Some(DM_JANET));
    channel
        .send_question("janet", "Q?")
        .await
        .expect("bound route sends");
    assert_eq!(
        api_requests(&server).await[0].url.path(),
        format!("/v1/{DM_JANET}/messages")
    );
}

#[tokio::test]
async fn inbound_without_open_question_is_dropped_and_audited() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    mount_create(&server).await;
    let project = Project::committed(THREE_ROUTES);
    let channel = project.channel(&server);
    bootstrap(&channel, JANET, DM_JANET);
    let q = channel
        .send_question("janet", "Ship it?")
        .await
        .expect("sent");
    assert_eq!(q.id, 1);

    let report = channel
        .process_batch(&[
            dm("n1", JANET, DM_JANET, "yes"),
            dm("n2", JANET, DM_JANET, "[Q-99] yes"),
            dm("n3", JANET, DM_JANET, "[Q-1x] [Q-] [q-1] yes"),
            message(
                "n4",
                JANET,
                DM_JANET,
                "DIRECT_MESSAGE",
                "yes",
                Some("spaces/DMJANET/threads/OTHER"),
            ),
        ])
        .expect("batch");
    assert_eq!(
        report.outcomes,
        [
            dropped("no_open_question"),
            dropped("no_open_question"),
            dropped("no_open_question"),
            dropped("no_open_question")
        ]
    );
    assert_eq!(report.ack_ids, ["n1", "n2", "n3", "n4"]);
    assert!(
        channel.question(1).expect("q1").is_open(),
        "an answer was recorded"
    );
    let drops = project
        .audit_lines()
        .into_iter()
        .filter(|l| l["event"] == "inbound_dropped" && l["reason"] == "no_open_question")
        .count();
    assert_eq!(drops, 4);
}

#[tokio::test]
async fn reply_resolves_exactly_that_question() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    mount_create(&server).await;
    let project = Project::committed(THREE_ROUTES);
    let channel = project.channel(&server);
    bootstrap(&channel, JANET, DM_JANET);
    let q1 = channel.send_question("janet", "First?").await.expect("q1");
    let q2 = channel.send_question("janet", "Second?").await.expect("q2");
    assert_eq!((q1.id, q2.id), (1, 2));
    let texts: Vec<String> = api_requests(&server)
        .await
        .iter()
        .map(|r| {
            serde_json::from_slice::<serde_json::Value>(&r.body).expect("json")["text"].to_string()
        })
        .collect();
    assert_eq!(texts, ["\"[Q-1] First?\"", "\"[Q-2] Second?\""]);

    // Token binding: Q-2 resolves, Q-1 stays open.
    let report = channel
        .process_batch(&[dm("r1", JANET, DM_JANET, "re [Q-2]: yes")])
        .expect("batch");
    assert_eq!(
        report.outcomes,
        [InboundOutcome::Answered {
            question_id: 2,
            route: "janet".into()
        }]
    );
    assert!(channel.question(1).expect("q1").is_open());
    let answer = channel.question(2).expect("q2").answer.expect("answered");
    assert_eq!(answer.text, "re [Q-2]: yes");
    assert_eq!(answer.sender, JANET);

    // A second reply to Q-2 is dropped; the first answer stands.
    let report = channel
        .process_batch(&[dm("r2", JANET, DM_JANET, "[Q-2] no, wait")])
        .expect("batch");
    assert_eq!(report.outcomes, [dropped("already_resolved")]);
    assert_eq!(report.ack_ids, ["r2"]);
    assert_eq!(
        channel.question(2).expect("q2").answer.expect("a").text,
        "re [Q-2]: yes"
    );
    assert!(project.audit_text().contains("already_resolved"));

    // Thread binding: a reply in Q-1's thread resolves Q-1 even with Q-2's
    // token in the text, because the thread name takes precedence.
    let thread = q1.thread_name.clone().expect("create returned a thread");
    let report = channel
        .process_batch(&[message(
            "r3",
            JANET,
            DM_JANET,
            "DIRECT_MESSAGE",
            "ok [Q-2]",
            Some(&thread),
        )])
        .expect("batch");
    assert_eq!(
        report.outcomes,
        [InboundOutcome::Answered {
            question_id: 1,
            route: "janet".into()
        }]
    );
    assert_eq!(
        channel.question(1).expect("q1").answer.expect("a").text,
        "ok [Q-2]"
    );
}

#[tokio::test]
async fn reply_from_non_recipient_in_bound_space_is_dropped() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    mount_create(&server).await;
    let project = Project::committed(THREE_ROUTES);
    let channel = project.channel(&server);
    bootstrap(&channel, JANET, DM_JANET);
    let q = channel
        .send_question("janet", "Ship it?")
        .await
        .expect("sent");
    let thread = q.thread_name.expect("thread");

    let report = channel
        .process_batch(&[
            // Not any route's recipient, in Janet's DM and Q-1's thread.
            message(
                "x1",
                "eve@example.com",
                DM_JANET,
                "DIRECT_MESSAGE",
                "[Q-1] yes",
                Some(&thread),
            ),
            // Another route's recipient, in Janet's DM.
            dm("x2", ASKER, DM_JANET, "[Q-1] yes"),
        ])
        .expect("batch");
    assert_eq!(
        report.outcomes,
        [
            dropped("sender_not_recipient"),
            dropped("space_bound_to_other_route")
        ]
    );
    assert!(channel.question(1).expect("q1").is_open());
    let senders: Vec<_> = project
        .audit_lines()
        .into_iter()
        .filter(|l| l["event"] == "inbound_dropped")
        .map(|l| l["sender"].clone())
        .collect();
    assert!(senders.contains(&json!("eve@example.com")), "{senders:?}");
    assert!(senders.contains(&json!(ASKER)), "{senders:?}");
}

#[tokio::test]
async fn unparseable_message_is_audited_before_ack() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    let project = Project::committed(THREE_ROUTES);
    let channel = project.channel(&server);
    Mock::given(method("POST"))
        .and(path(format!("/v1/{SUBSCRIPTION}:pull")))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"receivedMessages": [
                {"ackId": "bad-1", "message": {"data": "%%% not base64 %%%", "messageId": "m1"}}
            ]})),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("/v1/{SUBSCRIPTION}:acknowledge")))
        .and(body_json(json!({"ackIds": ["bad-1"]})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&server)
        .await;
    let report = channel.poll_once(10).await.expect("poll");
    assert_eq!(report.ack_ids, ["bad-1"]);
    let lines = project.audit_lines();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["event"], "inbound_unparseable");
    assert_eq!(lines[0]["message_id"], "m1");
    assert_eq!(lines[0]["reason"], "unparseable_base64");

    // When the audit line cannot be written, the message is never acked.
    let audit = project.dir().join(".trusty-channels/state/audit.jsonl");
    std::fs::remove_file(&audit).expect("rm audit");
    std::fs::create_dir(&audit).expect("make audit unwritable");
    let report = channel
        .process_batch(&[pulled("bad-2", Err(EventParseError::Base64))])
        .expect("batch");
    assert!(report.ack_ids.is_empty(), "acked silently: {report:?}");
    assert_eq!(report.withheld, ["bad-2"]);
}

#[tokio::test]
async fn add_on_format_batch_is_a_loud_error() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    let project = Project::committed(THREE_ROUTES);
    let channel = project.channel(&server);
    let add_on = |ack: &str| pulled(ack, Err(EventParseError::MissingField("type")));
    Mock::given(method("POST"))
        .and(path(format!("/v1/{SUBSCRIPTION}:pull")))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"receivedMessages": [
                {"ackId": "ao-1", "message": {"data": "eyJjaGF0Ijp7fX0=", "messageId": "m1"}},
                {"ackId": "ao-2", "message": {"data": "eyJjaGF0Ijp7fX0=", "messageId": "m2"}}
            ]})),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("/v1/{SUBSCRIPTION}:acknowledge")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(0)
        .mount(&server)
        .await;

    let err = channel.poll_once(10).await.expect_err("add-on format");
    assert!(
        matches!(err, InboundError::AddOnEventFormat { count: 2 }),
        "{err:?}"
    );
    assert!(err.to_string().contains("Workspace add-on"), "{err}");
    let lines = project.audit_lines();
    assert_eq!(lines.len(), 2, "each add-on event is audit-logged");
    assert!(lines.iter().all(|l| l["reason"] == "missing_event_type"));

    // One add-on event among parseable ones is an ordinary unparseable message.
    let report = channel
        .process_batch(&[
            add_on("ao-3"),
            dm("ok-1", "eve@example.com", "spaces/X", "hi"),
        ])
        .expect("mixed batch is not the add-on error");
    assert_eq!(report.ack_ids, ["ao-3", "ok-1"]);
}

#[tokio::test]
async fn ledger_write_failure_on_a_reply_withholds_its_ack() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    mount_create(&server).await;
    let project = Project::committed(THREE_ROUTES);
    let channel = project.channel(&server);
    bootstrap(&channel, JANET, DM_JANET);
    channel
        .send_question("janet", "Ship it?")
        .await
        .expect("q1");
    let ledger = project.dir().join(".trusty-channels/state/questions.jsonl");
    let mode = |m: u32| {
        std::fs::set_permissions(&ledger, std::fs::Permissions::from_mode(m)).expect("chmod");
    };

    mode(0o444);
    let report = channel
        .process_batch(&[dm("r1", JANET, DM_JANET, "[Q-1] yes")])
        .expect("batch");
    assert_eq!(report.outcomes, [dropped("state_write_failed")]);
    assert!(report.ack_ids.is_empty(), "acked an unrecorded answer");
    assert_eq!(report.withheld, ["r1"]);
    assert!(channel.question(1).expect("q1").is_open());

    // Redelivered once the ledger is writable again: the answer binds.
    mode(0o644);
    let report = channel
        .process_batch(&[dm("r1", JANET, DM_JANET, "[Q-1] yes")])
        .expect("batch");
    assert_eq!(report.ack_ids, ["r1"]);
    assert!(channel.question(1).expect("q1").answer.is_some());
}

#[tokio::test]
async fn bot_sender_is_dropped_before_route_lookup() {
    let server = MockServer::start().await;
    let project = Project::committed(THREE_ROUTES);
    let channel = project.channel(&server);
    // A bot whose email equals a route's recipient, in a DM.
    let mut bot = dm("b1", JANET, DM_JANET, "hello");
    if let Ok(ChatEvent::Message(m)) = &mut bot.event {
        m.sender.user_type = Some("BOT".into());
    }
    let report = channel.process_batch(&[bot]).expect("batch");
    assert_eq!(report.outcomes, [dropped("sender_is_bot")]);
    assert_eq!(
        channel.health().routes[0].space,
        None,
        "a bot taught a space"
    );
    assert!(project.audit_text().contains("sender_is_bot"));
}
