//! S1 `create_message` tests (#9448), moved from `tests/gchat_http.rs` when
//! the call became crate-private under ruling 7. Bodies are unchanged.

use serde_json::json;
use wiremock::matchers::{body_partial_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::{client, mount_token_expect as mount_token, token, token_requests};
use crate::gchat::api::client::{CreateMessage, MessageReplyOption};
use crate::gchat::api::error::GchatError;

#[tokio::test]
async fn unauthorized_chat_call_drops_the_cached_token() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().expect("tempdir");
    mount_token(&server, 3600, 2).await;
    // First Chat call: 401 (a revoked token). Later calls succeed.
    Mock::given(method("POST"))
        .and(path("/v1/spaces/AAAA/messages"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": {"code": 401, "message": "Request had invalid authentication credentials."}
        })))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/spaces/AAAA/messages"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"name": "spaces/AAAA/messages/M3"})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let client = client(&server, dir.path());
    let request = CreateMessage {
        space: "spaces/AAAA".into(),
        text: "hi".into(),
        ..CreateMessage::default()
    };
    let err = client.create_message(&request).await.expect_err("401");
    assert!(
        matches!(err, GchatError::Http { status: 401, .. }),
        "{err:?}"
    );
    client
        .create_message(&request)
        .await
        .expect("retry succeeds");
    assert_eq!(
        token_requests(&server).await.len(),
        2,
        "the 401 did not drop the cached token"
    );
}

#[tokio::test]
async fn create_message_with_thread_key_defaults_to_fallback_reply() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().expect("tempdir");
    mount_token(&server, 3600, 1).await;
    Mock::given(method("POST"))
        .and(path("/v1/spaces/AAAA/messages"))
        .and(query_param(
            "messageReplyOption",
            "REPLY_MESSAGE_FALLBACK_TO_NEW_THREAD",
        ))
        .and(body_partial_json(json!({"thread": {"threadKey": "q-9"}})))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"name": "spaces/AAAA/messages/M4"})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let request = CreateMessage {
        space: "spaces/AAAA".into(),
        text: "Question?".into(),
        thread_key: Some("q-9".into()),
        ..CreateMessage::default()
    };
    client(&server, dir.path())
        .create_message(&request)
        .await
        .expect("a keyed message without a reply option must default to fallback");
}

#[tokio::test]
async fn create_message_sends_bearer_path_and_thread_key() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().expect("tempdir");
    mount_token(&server, 3600, 1).await;
    Mock::given(method("POST"))
        .and(path("/v1/spaces/AAAA/messages"))
        .and(header(
            "authorization",
            format!("Bearer {}", token()).as_str(),
        ))
        .and(query_param(
            "messageReplyOption",
            "REPLY_MESSAGE_FALLBACK_TO_NEW_THREAD",
        ))
        .and(query_param("requestId", "req-1"))
        .and(body_partial_json(
            json!({"text": "Ship it?", "thread": {"threadKey": "q-42"}}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "name": "spaces/AAAA/messages/M1",
            "text": "Ship it?",
            "thread": {"name": "spaces/AAAA/threads/T1", "threadKey": "q-42"}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let sent = client(&server, dir.path())
        .create_message(&CreateMessage {
            space: "spaces/AAAA".into(),
            text: "Ship it?".into(),
            thread_key: Some("q-42".into()),
            reply_option: Some(MessageReplyOption::ReplyFallbackToNewThread),
            request_id: Some("req-1".into()),
        })
        .await
        .expect("create message");
    assert_eq!(sent.name, "spaces/AAAA/messages/M1");
    assert_eq!(
        sent.thread.map(|t| t.name).as_deref(),
        Some("spaces/AAAA/threads/T1")
    );
}

#[tokio::test]
async fn create_message_non_2xx_is_typed_and_never_contains_the_token() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().expect("tempdir");
    mount_token(&server, 3600, 1).await;
    // A hostile body that echoes the bearer token back.
    Mock::given(method("POST"))
        .and(path("/v1/spaces/AAAA/messages"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({
            "error": {"code": 403, "status": "PERMISSION_DENIED",
                      "message": format!("caller {} lacks access", token())}
        })))
        .mount(&server)
        .await;
    let request = CreateMessage {
        space: "spaces/AAAA".into(),
        text: "hi".into(),
        ..CreateMessage::default()
    };
    let err = client(&server, dir.path())
        .create_message(&request)
        .await
        .expect_err("403 must be an error");
    assert!(
        matches!(
            err,
            GchatError::Http {
                api: "chat",
                status: 403,
                ..
            }
        ),
        "{err:?}"
    );
    for text in [err.to_string(), format!("{err:?}")] {
        assert!(!text.contains(token()), "token leaked: {text}");
        assert!(
            text.contains("lacks access"),
            "Google's message dropped: {text}"
        );
    }
}
