//! Hermetic tests for the gchat route layer (#9448 S2a), plus the S1
//! `create_message` and token tests that moved here when those calls became
//! crate-private (ruling 7).
//!
//! Why: each acceptance criterion of #9448 S2a needs a test a wrong
//! implementation fails, against a real git repo and a mock Chat server.
//! What: shared fixtures — a run-time RSA key, a temp project committed to a
//! temp git repo, a wiremock server standing in for OAuth, Chat and Pub/Sub,
//! and event builders. No test reaches Google.
//! Test: this module is the test.

#![cfg(unix)]

mod auth;
mod client_send;
mod default_branch;
mod doctor;
mod egress;
mod inbound;
mod ledger;
mod routes_load;
mod server;
mod space_routes;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use aws_lc_rs::encoding::AsDer;
use aws_lc_rs::rsa::{KeyPair, KeySize};
use aws_lc_rs::signature::KeyPair as _;
use base64::Engine as _;
use serde_json::json;
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

use crate::gchat::api::client::{Endpoints, GchatClient};
use crate::gchat::api::events::{ChatEvent, MessageEvent, PulledMessage, Sender, SpaceInfo};
use crate::gchat::channel::GchatChannel;

pub(super) const SA_EMAIL: &str = "bot@test-project.iam.gserviceaccount.com";
pub(super) const SUBSCRIPTION: &str = "projects/test-project/subscriptions/chat-in";
pub(super) const JANET: &str = "janet@example.com";
pub(super) const REVIEWER: &str = "rev@example.com";
pub(super) const ASKER: &str = "ask@example.com";
pub(super) const DM_JANET: &str = "spaces/DMJANET";

/// Three routes: `janet` allows both kinds, `notices` only review_notice,
/// `asker` only question. `{KEY}` is replaced with the key-file path.
pub(super) const THREE_ROUTES: &str = r#"version = 1

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

[[gchat.routes]]
name = "asker"
recipient = "ask@example.com"
kinds = ["question"]
"#;

/// The mock access token. Built at run time so no `ya29.` literal is
/// committed for a credential scanner to match (#9448 review).
pub(super) fn token() -> &'static str {
    static TOKEN: OnceLock<String> = OnceLock::new();
    TOKEN.get_or_init(|| ["ya29", "test-access-token-0123456789"].join("."))
}

/// One PEM armour line, e.g. `pem_line("BEGIN", "PUBLIC KEY")`. Assembled at
/// run time so no private-key header literal is committed (#9448 review).
pub(super) fn pem_line(edge: &str, label: &str) -> String {
    format!("-----{edge} {label}-----")
}

fn pem(label: &str, der: &[u8]) -> String {
    let b64 = base64::engine::general_purpose::STANDARD.encode(der);
    let body: Vec<&str> = b64
        .as_bytes()
        .chunks(64)
        .map(|c| std::str::from_utf8(c).expect("base64 is ASCII"))
        .collect();
    format!(
        "{}\n{}\n{}\n",
        pem_line("BEGIN", label),
        body.join("\n"),
        pem_line("END", label)
    )
}

/// A throwaway RSA-2048 key, generated once per run: (PKCS#8 private PEM,
/// SPKI public PEM).
pub(super) fn throwaway_key() -> &'static (String, String) {
    static KEY: OnceLock<(String, String)> = OnceLock::new();
    KEY.get_or_init(|| {
        let pair = KeyPair::generate(KeySize::Rsa2048).expect("generate RSA key");
        let private = pair.as_der().expect("PKCS#8 DER");
        let public = pair.public_key().as_der().expect("SPKI DER");
        (
            pem("PRIVATE KEY", private.as_ref()),
            pem("PUBLIC KEY", public.as_ref()),
        )
    })
}

/// Mount the token endpoint, expecting exactly `expect` calls.
pub(super) async fn mount_token_expect(server: &MockServer, expires_in: u64, expect: u64) {
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": token(), "expires_in": expires_in, "token_type": "Bearer"
        })))
        .expect(expect)
        .mount(server)
        .await;
}

/// Every request the mock server saw on the token endpoint.
pub(super) async fn token_requests(server: &MockServer) -> Vec<Request> {
    let all = server.received_requests().await.expect("recording on");
    all.into_iter()
        .filter(|r| r.url.path() == "/token")
        .collect()
}

/// Write a Google-shaped JSON key file with permission bits `mode`.
pub(super) fn write_key_file(dir: &Path, mode: u32) -> PathBuf {
    std::fs::create_dir_all(dir).expect("key dir");
    let path = dir.join("sa.json");
    let key = json!({
        "type": "service_account",
        "project_id": "test-project",
        "private_key_id": "kid-1",
        "private_key": throwaway_key().0,
        "client_email": SA_EMAIL,
        "token_uri": "https://oauth2.googleapis.com/token",
    });
    std::fs::write(&path, key.to_string()).expect("write key file");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).expect("chmod");
    path
}

pub(super) async fn mount_token(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": token(), "expires_in": 3600, "token_type": "Bearer"
        })))
        .mount(server)
        .await;
}

pub(super) fn client(server: &MockServer, dir: &Path) -> GchatClient {
    let key = write_key_file(dir, 0o600);
    GchatClient::with_endpoints(&key, Endpoints::single_host(&server.uri())).expect("client")
}

/// Mount Chat `messages.create`: the n-th call returns message `M{n}` in
/// thread `T{n}` of the requested space.
pub(super) async fn mount_create(server: &MockServer) {
    let n = Arc::new(AtomicU64::new(1));
    Mock::given(method("POST"))
        .and(path_regex(r"^/v1/spaces/[A-Za-z0-9]+/messages$"))
        .respond_with(move |req: &Request| {
            let i = n.fetch_add(1, Ordering::SeqCst);
            let space = req
                .url
                .path()
                .trim_start_matches("/v1/")
                .trim_end_matches("/messages")
                .to_string();
            ResponseTemplate::new(200).set_body_json(json!({
                "name": format!("{space}/messages/M{i}"),
                "thread": {"name": format!("{space}/threads/T{i}")}
            }))
        })
        .mount(server)
        .await;
}

/// Every request the mock server saw whose path is not `/token`.
pub(super) async fn api_requests(server: &MockServer) -> Vec<Request> {
    let all = server.received_requests().await.expect("recording on");
    all.into_iter()
        .filter(|r| r.url.path() != "/token")
        .collect()
}

/// Every request the mock server saw, the token endpoint included.
pub(super) async fn all_requests(server: &MockServer) -> usize {
    server
        .received_requests()
        .await
        .expect("recording on")
        .len()
}

/// Run git in `dir` with a fixed identity and no hooks or signing.
pub(super) fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(dir)
        .args([
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@example.com",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .expect("run git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A temp project in its own git repo with a key file outside the repo.
pub(super) struct Project {
    pub(super) root: tempfile::TempDir,
    key: PathBuf,
    /// Held so the key file outlives the project.
    _key_dir: tempfile::TempDir,
}

impl Project {
    /// A project with `routes` (with `{KEY}` filled in) committed and clean.
    pub(super) fn committed(routes: &str) -> Self {
        let project = Self::uncommitted(routes);
        git(project.dir(), &["add", ".trusty-channels/routes.toml"]);
        git(project.dir(), &["commit", "-q", "-m", "routes"]);
        project
    }

    /// A project in a fresh git repo on `main`, with one empty root commit
    /// and `routes` written but untracked.
    pub(super) fn uncommitted(routes: &str) -> Self {
        let root = tempfile::tempdir().expect("project dir");
        let key_dir = tempfile::tempdir().expect("key dir");
        let key = write_key_file(key_dir.path(), 0o600);
        // #8454 Db1: on a default branch whatever `init.defaultBranch` says;
        // the root commit makes `main` exist before routes are committed.
        git(root.path(), &["init", "-q", "-b", "main"]);
        git(
            root.path(),
            &["commit", "-q", "--allow-empty", "-m", "root"],
        );
        let config = root.path().join(".trusty-channels");
        std::fs::create_dir_all(&config).expect("config dir");
        let text = routes.replace("{KEY}", &key.display().to_string());
        std::fs::write(config.join("routes.toml"), text).expect("write routes");
        Self {
            root,
            key,
            _key_dir: key_dir,
        }
    }

    /// The service-account key file, outside the repo.
    pub(super) fn key_file(&self) -> &Path {
        &self.key
    }

    pub(super) fn dir(&self) -> &Path {
        self.root.path()
    }

    pub(super) fn routes_file(&self) -> PathBuf {
        self.dir().join(".trusty-channels/routes.toml")
    }

    pub(super) fn channel(&self, server: &MockServer) -> GchatChannel {
        GchatChannel::open_with(self.dir(), Endpoints::single_host(&server.uri()))
            .expect("open channel")
    }

    /// The audit log text (empty when absent).
    pub(super) fn audit_text(&self) -> String {
        std::fs::read_to_string(self.dir().join(".trusty-channels/state/audit.jsonl"))
            .unwrap_or_default()
    }

    /// Parsed audit lines.
    pub(super) fn audit_lines(&self) -> Vec<serde_json::Value> {
        self.audit_text()
            .lines()
            .map(|l| serde_json::from_str(l).expect("audit line is JSON"))
            .collect()
    }
}

/// A pulled MESSAGE event.
pub(super) fn message(
    ack: &str,
    sender: &str,
    space: &str,
    space_type: &str,
    text: &str,
    thread: Option<&str>,
) -> PulledMessage {
    let event = MessageEvent {
        event_time: None,
        message_name: format!("{space}/messages/in-{ack}"),
        text: text.to_string(),
        argument_text: None,
        thread_name: thread.map(str::to_string),
        thread_key: None,
        space: SpaceInfo {
            name: space.to_string(),
            space_type: Some(space_type.to_string()),
            ..SpaceInfo::default()
        },
        sender: Sender {
            name: format!("users/{sender}"),
            email: Some(sender.to_string()),
            ..Sender::default()
        },
    };
    pulled(ack, Ok(ChatEvent::Message(Box::new(event))))
}

/// A DM message from `sender` in `space`.
pub(super) fn dm(ack: &str, sender: &str, space: &str, text: &str) -> PulledMessage {
    message(ack, sender, space, "DIRECT_MESSAGE", text, None)
}

pub(super) fn pulled(
    ack: &str,
    event: Result<ChatEvent, crate::gchat::api::error::EventParseError>,
) -> PulledMessage {
    PulledMessage {
        ack_id: ack.to_string(),
        message_id: format!("id-{ack}"),
        publish_time: None,
        attributes: Default::default(),
        delivery_attempt: None,
        event,
    }
}

/// Bind `route`'s recipient to `space` through a bootstrap DM.
pub(super) fn bootstrap(channel: &GchatChannel, recipient: &str, space: &str) {
    channel
        .process_batch(&[dm("boot", recipient, space, "hello")])
        .expect("bootstrap batch");
}
