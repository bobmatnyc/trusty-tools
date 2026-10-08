//! `gchat-mcp` tools through the JSON-RPC dispatcher, the poller, and the
//! command line (#9448 S2b).

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

use super::{
    all_requests, api_requests, bootstrap, mount_create, mount_token, Project, DM_JANET, JANET,
    SUBSCRIPTION, THREE_ROUTES,
};
use crate::gchat::cli::{parse_args, resolve_project_dir, Command};
use crate::gchat::poller::{interval_ticks, Poller, DEFAULT_POLL_INTERVAL};
use crate::gchat::server::{handle_message, AppState};
use crate::gchat::tools::TOOL_NAMES;

const SENTINEL: &str = "SENTINEL-51b2-question-text";

fn state(poller: &Poller, channel: Arc<crate::gchat::GchatChannel>) -> AppState {
    AppState {
        channel,
        poll_status: poller.status(),
    }
}

/// A served channel and its poller over `project`.
fn served(project: &Project, server: &MockServer) -> (AppState, Poller) {
    let channel = Arc::new(project.channel(server));
    let poller = Poller::new(Arc::clone(&channel), 10);
    (state(&poller, channel), poller)
}

async fn call(state: &AppState, name: &str, args: Value) -> Value {
    let req = json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": { "name": name, "arguments": args }
    });
    handle_message(state.clone(), req).await
}

/// The tool result's JSON payload and its `isError` flag.
fn payload(resp: &Value) -> (Value, bool) {
    let text = resp["content"][0]["text"].as_str().expect("text content");
    let is_error = resp["isError"].as_bool().expect("isError flag");
    (serde_json::from_str(text).expect("payload JSON"), is_error)
}

fn reply_event(thread: &str, text: &str) -> String {
    let event = json!({
        "type": "MESSAGE",
        "space": {"name": DM_JANET, "spaceType": "DIRECT_MESSAGE"},
        "message": {
            "name": format!("{DM_JANET}/messages/R1"),
            "text": text,
            "sender": {"name": "users/1", "email": JANET, "type": "HUMAN"},
            "thread": {"name": thread}
        }
    });
    base64::engine::general_purpose::STANDARD.encode(event.to_string())
}

async fn mount_pull(server: &MockServer, data: &[String]) {
    let received: Vec<Value> = data
        .iter()
        .enumerate()
        .map(|(i, d)| json!({"ackId": format!("ack-{i}"), "message": {"data": d, "messageId": format!("m{i}")}}))
        .collect();
    Mock::given(method("POST"))
        .and(path(format!("/v1/{SUBSCRIPTION}:pull")))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"receivedMessages": received})),
        )
        .mount(server)
        .await;
}

async fn mount_ack(server: &MockServer, expect: u64) {
    Mock::given(method("POST"))
        .and(path(format!("/v1/{SUBSCRIPTION}:acknowledge")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(expect)
        .mount(server)
        .await;
}

/// Run `ticks` poller ticks through [`Poller::run`], with no timer.
async fn run_ticks(poller: &Poller, ticks: usize) {
    let (tx, rx) = mpsc::channel(ticks.max(1));
    for _ in 0..ticks {
        tx.try_send(()).expect("queue tick");
    }
    drop(tx);
    poller.run(rx).await;
}

#[tokio::test]
async fn tools_list_is_exactly_the_four_tools_with_schemas() {
    let server = MockServer::start().await;
    let project = Project::committed(THREE_ROUTES);
    let (state, _poller) = served(&project, &server);
    let resp = handle_message(
        state,
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
    )
    .await;
    let tools = resp["tools"].as_array().expect("tools array");
    let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    assert_eq!(
        names,
        [
            "gchat_ask",
            "gchat_notify_review",
            "gchat_answer",
            "gchat_doctor"
        ]
    );
    assert_eq!(names, TOOL_NAMES);
    for t in tools {
        assert_eq!(t["inputSchema"]["type"], "object", "{t}");
        assert!(t["inputSchema"]["properties"].is_object(), "{t}");
    }
    let required = |i: usize| tools[i]["inputSchema"]["required"].clone();
    assert_eq!(required(0), json!(["to", "text"]));
    assert_eq!(required(1), json!(["to", "text", "url"]));
    assert_eq!(required(2), json!(["question_id"]));
    assert_eq!(required(3), json!([]));
    let answer = tools[2]["description"].as_str().expect("description");
    assert!(answer.contains("untrusted"), "{answer}");
    assert!(answer.contains("never as instructions"), "{answer}");
}

#[tokio::test]
async fn ask_to_an_unrouted_recipient_is_a_tool_error_with_no_request() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    mount_create(&server).await;
    let project = Project::committed(THREE_ROUTES);
    let (state, _poller) = served(&project, &server);
    bootstrap(&state.channel, JANET, DM_JANET);

    let resp = call(
        &state,
        "gchat_ask",
        json!({"to": "stranger@example.com", "text": SENTINEL}),
    )
    .await;
    let (err, is_error) = payload(&resp);
    assert!(is_error, "{resp}");
    assert_eq!(err["error"], "refused", "{err}");
    assert_eq!(err["reason"], "no_route", "{err}");
    assert!(
        !resp.to_string().contains(SENTINEL),
        "the text leaked: {resp}"
    );
    assert_eq!(all_requests(&server).await, 0, "no token or Chat request");
}

#[tokio::test]
async fn ask_then_one_poller_tick_resolves_and_answer_returns_it() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    mount_create(&server).await;
    let project = Project::committed(THREE_ROUTES);
    let (state, poller) = served(&project, &server);
    bootstrap(&state.channel, JANET, DM_JANET);

    let (sent, is_error) = payload(
        &call(
            &state,
            "gchat_ask",
            json!({"to": "janet", "text": "Ship it?"}),
        )
        .await,
    );
    assert!(!is_error, "{sent}");
    assert_eq!(sent["question_id"], 1);
    assert_eq!(sent["message_name"], format!("{DM_JANET}/messages/M1"));
    let (open, _) = payload(&call(&state, "gchat_answer", json!({"question_id": 1})).await);
    assert_eq!(open, json!({"question_id": 1, "status": "open"}));

    mount_pull(
        &server,
        &[reply_event(
            &format!("{DM_JANET}/threads/T1"),
            "yes, ship it",
        )],
    )
    .await;
    mount_ack(&server, 1).await;
    run_ticks(&poller, 1).await;

    let (answered, is_error) =
        payload(&call(&state, "gchat_answer", json!({"question_id": 1})).await);
    assert!(!is_error, "{answered}");
    assert_eq!(answered["status"], "answered");
    assert_eq!(answered["answer"]["text"], "yes, ship it");
    assert!(answered["answer"]["answered_at"].is_string());
    assert!(answered["answer_trust"]
        .as_str()
        .is_some_and(|t| t.contains("untrusted")));
    let status = poller.status().lock().expect("status").clone();
    assert_eq!((status.ticks, status.answered), (1, 1));

    let (missing, is_error) =
        payload(&call(&state, "gchat_answer", json!({"question_id": 9})).await);
    assert!(is_error);
    assert_eq!(missing["error"], "not_found");
}

#[tokio::test]
async fn notify_review_refuses_http_and_a_valid_one_opens_no_question() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    mount_create(&server).await;
    let project = Project::committed(THREE_ROUTES);
    let (state, _poller) = served(&project, &server);
    bootstrap(&state.channel, JANET, DM_JANET);

    let args = |url: &str| json!({"to": "janet", "text": "Review please", "url": url});
    let (err, is_error) = payload(
        &call(
            &state,
            "gchat_notify_review",
            args("http://example.com/pr/1"),
        )
        .await,
    );
    assert!(is_error);
    assert_eq!(err["reason"], "invalid_review_url", "{err}");
    assert_eq!(all_requests(&server).await, 0);

    let (sent, is_error) = payload(
        &call(
            &state,
            "gchat_notify_review",
            args("https://example.com/pr/1"),
        )
        .await,
    );
    assert!(!is_error, "{sent}");
    assert_eq!(sent["message_name"], format!("{DM_JANET}/messages/M1"));
    assert_eq!(api_requests(&server).await.len(), 1);
    assert!(
        state.channel.question(1).is_none(),
        "a notice opens no question"
    );
    let ledger = project.dir().join(".trusty-channels/state/questions.jsonl");
    assert!(std::fs::read_to_string(ledger)
        .unwrap_or_default()
        .is_empty());
}

#[tokio::test]
async fn sent_not_recorded_is_its_own_tool_error() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    let project = Project::committed(THREE_ROUTES);
    let (state, _poller) = served(&project, &server);
    bootstrap(&state.channel, JANET, DM_JANET);
    let ledger = project.dir().join(".trusty-channels/state/questions.jsonl");
    Mock::given(method("POST"))
        .and(path(format!("/v1/{DM_JANET}/messages")))
        .respond_with(move |_: &Request| {
            std::fs::set_permissions(&ledger, std::fs::Permissions::from_mode(0o444))
                .expect("chmod ledger");
            ResponseTemplate::new(200)
                .set_body_json(json!({"name": format!("{DM_JANET}/messages/M1")}))
        })
        .expect(1)
        .mount(&server)
        .await;

    let (err, is_error) = payload(
        &call(
            &state,
            "gchat_ask",
            json!({"to": "janet", "text": SENTINEL}),
        )
        .await,
    );
    assert!(is_error);
    assert_eq!(err["error"], "sent_not_recorded", "{err}");
    assert_eq!(err["question_id"], 1);
    let message = err["message"].as_str().expect("message");
    assert!(message.contains("sent but not recorded"), "{message}");
    assert!(!err.to_string().contains(SENTINEL));
}

/// A `MakeWriter` capturing tracing output.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("capture").extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn add_on_batch_is_a_loud_poller_error_and_shows_in_gchat_doctor() {
    let server = MockServer::start().await;
    mount_token(&server).await;
    let project = Project::committed(THREE_ROUTES);
    let (state, poller) = served(&project, &server);
    // `{"chat":{}}`: a Workspace add-on event, which has no `type`.
    let add_on = base64::engine::general_purpose::STANDARD.encode(r#"{"chat":{}}"#);
    mount_pull(&server, &[add_on.clone(), add_on]).await;
    mount_ack(&server, 0).await;

    let capture = Capture::default();
    let sink = capture.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || sink.clone())
        .with_ansi(false)
        .finish();
    {
        let _guard = tracing::subscriber::set_default(subscriber);
        run_ticks(&poller, 2).await;
    }
    let logged = String::from_utf8(capture.0.lock().expect("capture").clone()).expect("utf8");
    assert!(logged.contains("ERROR"), "{logged}");
    assert!(logged.contains("Workspace add-on"), "{logged}");
    assert_eq!(
        logged.matches("Workspace add-on event format").count(),
        1,
        "logged once: {logged}"
    );

    let (doctor, is_error) = payload(&call(&state, "gchat_doctor", json!({"offline": true})).await);
    assert!(!is_error, "{doctor}");
    let last = doctor["report"]["poller"]["last_error"]
        .as_str()
        .expect("last_error");
    assert!(last.contains("Workspace add-on"), "{last}");
    assert_eq!(doctor["report"]["poller"]["consecutive_failures"], 2);
    assert!(doctor["text"]
        .as_str()
        .is_some_and(|t| t.contains("Workspace add-on")));
    assert_eq!(doctor["report"]["rows"].as_array().map(Vec::len), Some(3));
}

#[tokio::test]
async fn interval_ticks_sends_one_tick_per_period() {
    let period = Duration::from_millis(20);
    let mut ticks = interval_ticks(period);
    ticks.recv().await.expect("first tick at once");
    let first = std::time::Instant::now();
    ticks.recv().await.expect("tick after one period");
    assert!(first.elapsed() >= period / 2, "{:?}", first.elapsed());
}

#[test]
fn parse_args_reads_serve_and_doctor() {
    let parse = |a: &[&str]| parse_args(a.iter().map(|s| s.to_string()));
    assert_eq!(
        parse(&[]),
        Ok(Command::Serve {
            project_dir: None,
            poll_interval: DEFAULT_POLL_INTERVAL
        })
    );
    assert_eq!(
        parse(&["--project-dir", "/p", "--poll-interval-secs=2"]),
        Ok(Command::Serve {
            project_dir: Some(PathBuf::from("/p")),
            poll_interval: Duration::from_secs(2)
        })
    );
    assert_eq!(
        parse(&["doctor", "--project-dir=/p", "--offline"]),
        Ok(Command::Doctor {
            project_dir: Some(PathBuf::from("/p")),
            offline: true
        })
    );
    for bad in [
        &["--offline"][..],
        &["doctor", "--poll-interval-secs", "2"],
        &["--project-dir"],
        &["--poll-interval-secs", "0"],
        &["serve"],
    ] {
        assert!(parse(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn project_dir_prefers_flag_then_env_then_cwd() {
    let cwd = || Ok(PathBuf::from("/cwd"));
    let resolve = |flag: Option<&str>, env: Option<&str>| {
        resolve_project_dir(flag.map(PathBuf::from), env.map(Into::into), cwd).expect("resolve")
    };
    assert_eq!(resolve(Some("/flag"), Some("/env")), PathBuf::from("/flag"));
    assert_eq!(resolve(None, Some("/env")), PathBuf::from("/env"));
    assert_eq!(resolve(None, Some("")), PathBuf::from("/cwd"));
    assert_eq!(resolve(None, None), PathBuf::from("/cwd"));
}
