//! Tests for the shared secrets client and `secrets_get_ref` (#7522).
//!
//! The tool tests drive `crate::mcp::dispatch` over the daemon's real
//! `StateBackend`, against an in-process trusty-secrets server on a socket in
//! a `TempDir`, backed by a `MemoryBackend`. No test opens the Keychain or
//! `~/.trusty-tools`, and the client's spawn program does not exist, so no
//! real `trusty-secrets` runs. Each tool call captures the JSON-RPC response
//! and TRACE tracing; [`Outcome::assert_no_value`] checks both.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use trusty_common::uds::UdsRpcError;
use trusty_secrets::api::methods::ScopeKind;
use trusty_secrets::server::{ClientError, ErrorKind, RpcFailure};
use trusty_secrets::{BackendId, SecretKey, VaultName};

use super::*;

/// The project vault of the tool fixture's `git@github.com:Acme/Web.git`.
const PROJECT_VAULT: &str = "trusty/acme/web";

#[test]
fn with_project_folds_the_directory_into_the_params() {
    let folded = with_project(Path::new("/repo"), json!({ "vault": "v" })).expect("utf-8");
    assert_eq!(folded, json!({ "vault": "v", "project": "/repo" }));
    let from_null = with_project(Path::new("/repo"), Value::Null).expect("utf-8");
    assert_eq!(from_null, json!({ "project": "/repo" }));
}

/// #7522: the CLI's text is unchanged by the move into the library, and a
/// transport error's own detail never reaches it.
#[test]
fn describe_keeps_the_cli_text_and_drops_transport_detail() {
    let socket = Path::new("/run/s.sock");
    let rpc = ClientError::Rpc(RpcFailure::new(
        -32054,
        "secrets.list: the key is not in that vault",
        Some(ErrorKind::NotFound),
    ));
    assert_eq!(
        describe("tm secrets", &rpc, socket),
        "tm secrets: secrets.list: the key is not in that vault"
    );
    assert_eq!(
        describe("tm secrets", &ClientError::HomeUnavailable, socket),
        "tm secrets: the home directory is unavailable"
    );
    assert_eq!(
        describe("tm secrets", &ClientError::EmptyResponse, socket),
        "tm secrets: trusty-secrets at /run/s.sock answered without a result"
    );
    let transport = ClientError::Transport(Box::new(UdsRpcError::Encode {
        path: PathBuf::from("/detail/DETAIL-SENTINEL"),
        source: serde_json::from_str::<Value>("{").expect_err("bad json"),
    }));
    let text = describe("secrets_get_ref", &transport, socket);
    assert_eq!(
        text,
        "secrets_get_ref: the request did not cross the trusty-secrets socket /run/s.sock"
    );
}

/// #7522: the answer's field set is exactly DOC-74 §10.2's — nothing that
/// could carry a value, a masked head or a length.
#[test]
fn secrets_get_ref_answer_has_exactly_the_documented_fields() {
    let answer = RefAnswer {
        reference: "secret://API_TOKEN".to_owned(),
        key: SecretKey::new("API_TOKEN").expect("key"),
        present: true,
        scope: Some(ScopeKind::Project),
        vault: Some(VaultName::new(PROJECT_VAULT).expect("vault")),
        backend: BackendId::keychain(),
        imported_at: Some(1),
    };
    let json = serde_json::to_value(&answer).expect("encode");
    let mut fields: Vec<&str> = json
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    fields.sort_unstable();
    assert_eq!(
        fields,
        [
            "backend",
            "imported_at",
            "key",
            "present",
            "reference",
            "scope",
            "vault"
        ]
    );
    assert_eq!(json["scope"], "project");
    assert_eq!(json["vault"], PROJECT_VAULT);
    assert_eq!(json["backend"], "keychain");
}

#[cfg(feature = "daemon")]
mod tool {
    //! `secrets_get_ref` end to end, through the daemon's `StateBackend`.
    //!
    //! Needs the `daemon` feature for `StateBackend` and `DaemonState`.

    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use serde_json::{Value, json};
    use tempfile::TempDir;
    use tokio::sync::oneshot;
    use trusty_secrets::api::methods::method;
    use trusty_secrets::server::{BackendFactory, OnDemandSecrets, ServerSettings, serve};
    use trusty_secrets::store::{MemoryBackend, SecretBackend};
    use trusty_secrets::{BackendId, SecretsError};

    use super::PROJECT_VAULT;
    use crate::secrets_client::with_project;

    /// A value longer than 8 characters. No output may carry any part of it,
    /// not even the 8-character head `secrets.set` confirms with.
    const VALUE: &str = "sk-live-TAIL-7522-c0ffee";
    /// The value's head and tail, each checked separately.
    const PARTS: [&str; 2] = ["sk-live-", "TAIL-7522-c0ffee"];
    /// The owner vault of the same remote.
    const OWNER_VAULT: &str = "trusty/acme";
    /// The harness machine config: selects the memory `keychain` on every OS.
    const PINNED_MACHINE_CONFIG: &str = "secrets:\n  default_backend: keychain\n";

    /// A served socket over one memory backend, in a temp dir.
    struct Harness {
        tmp: TempDir,
        repo: PathBuf,
        client: Arc<OnDemandSecrets>,
        _shutdown: oneshot::Sender<()>,
    }

    fn git(dir: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .expect("git runs");
        assert!(status.success(), "git {args:?} failed");
    }

    /// A client whose spawn fallback can never start a real server.
    fn client_at(tmp: &Path, socket: &Path) -> OnDemandSecrets {
        OnDemandSecrets::at(socket).with_program(tmp.join("no-such-trusty-secrets"))
    }

    async fn harness() -> Harness {
        let tmp = TempDir::new().expect("tempdir");
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).expect("mkdir repo");
        git(&repo, &["init", "-q"]);
        git(
            &repo,
            &["remote", "add", "origin", "git@github.com:Acme/Web.git"],
        );
        let settings = ServerSettings::new(
            tmp.path().join("run").join("s.sock"),
            tmp.path().join("index"),
            tmp.path().join("machine.yaml"),
            Duration::from_secs(60),
        );
        // #7522: with no machine config the server picks the build's default
        // backend, `keychain` on macOS but `file` elsewhere (#9326), and this
        // factory serves no `file`. Pin `keychain` so every OS selects it.
        std::fs::write(&settings.machine_config, PINNED_MACHINE_CONFIG)
            .expect("write machine.yaml");
        let pinned = trusty_secrets::store::config::load_machine_at(&settings.machine_config)
            .expect("load machine.yaml")
            .and_then(|m| m.default_backend);
        assert_eq!(
            pinned,
            Some(BackendId::keychain()),
            "the harness must pin the memory `keychain` backend on every OS"
        );
        let keychain = Arc::new(MemoryBackend::new());
        let backends: BackendFactory = Arc::new(move |id: &BackendId| {
            if id.as_str() != BackendId::KEYCHAIN {
                return Err(SecretsError::UnknownBackend {
                    backend: id.to_string(),
                });
            }
            Ok(Arc::clone(&keychain) as Arc<dyn SecretBackend>)
        });
        let (tx, rx) = oneshot::channel::<()>();
        let socket = settings.socket.clone();
        tokio::spawn(serve(settings, backends, async move {
            let _ = rx.await;
        }));
        let started = Instant::now();
        while !trusty_common::uds::socket_is_serving(&socket, Duration::from_millis(200)).await {
            assert!(
                started.elapsed() < Duration::from_secs(20),
                "socket never served"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let client = Arc::new(client_at(tmp.path(), &socket));
        Harness {
            tmp,
            repo,
            client,
            _shutdown: tx,
        }
    }

    /// Store [`VALUE`] under `key` in `vault` through the socket, as the console
    /// or `tm secrets set` would.
    async fn seed(h: &Harness, vault: &str, key: &str) {
        let params = with_project(
            &h.repo,
            json!({ "vault": vault, "key": key, "value": VALUE }),
        )
        .expect("utf-8");
        h.client.call(method::SET, params).await.expect("set");
    }

    /// Collects every tracing line written while the tool runs.
    #[derive(Clone, Default)]
    struct LogSink(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for LogSink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("log sink").extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// One tool call: the whole JSON-RPC response, its text block, and the logs.
    struct Outcome {
        response: String,
        text: String,
        is_error: bool,
        logs: String,
    }

    impl Outcome {
        /// Neither the response nor the logs carry any part of [`VALUE`].
        fn assert_no_value(&self) {
            for (what, text) in [("response", &self.response), ("logs", &self.logs)] {
                for part in PARTS {
                    assert!(!text.contains(part), "{what} carries the value: {text}");
                }
            }
        }

        /// The successful answer, decoded.
        fn answer(&self) -> Value {
            assert!(!self.is_error, "the tool failed: {}", self.text);
            serde_json::from_str(&self.text).expect("JSON answer")
        }
    }

    /// Call `secrets_get_ref` through `crate::mcp::dispatch` on the daemon's
    /// `StateBackend`, dialling `client`.
    async fn run_tool(client: &Arc<OnDemandSecrets>, tmp: &Path, args: Value) -> Outcome {
        use crate::daemon::mcp_backend::StateBackend;
        use crate::daemon::state::DaemonState;

        let state = Arc::new(DaemonState::with_root(tmp.join("mpm")));
        let backend = StateBackend::new(state).with_secrets_client(Arc::clone(client));
        let sink = LogSink::default();
        let writer = sink.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);
        let request = trusty_mcp::Request {
            jsonrpc: Some("2.0".into()),
            id: Some(json!(1)),
            method: "tools/call".into(),
            params: Some(json!({ "name": "secrets_get_ref", "arguments": args })),
        };
        let response = crate::mcp::dispatch(&backend, request).await;
        let response = serde_json::to_value(&response).expect("encode response");
        let result = &response["result"];
        let logs = String::from_utf8_lossy(&sink.0.lock().expect("log sink")).into_owned();
        Outcome {
            text: result["content"][0]["text"]
                .as_str()
                .expect("text block")
                .to_owned(),
            is_error: result["isError"] == true,
            response: response.to_string(),
            logs,
        }
    }

    fn args(project: &Path, key: &str) -> Value {
        json!({ "project": project.to_str().expect("utf-8"), "key": key })
    }

    /// #7522 closure condition: the tool resolves a stored key to its reference
    /// and metadata, and the value reaches neither the response nor the logs.
    #[tokio::test]
    async fn secrets_get_ref_reports_a_project_key_without_its_value() {
        let h = harness().await;
        seed(&h, PROJECT_VAULT, "API_TOKEN").await;

        let outcome = run_tool(&h.client, h.tmp.path(), args(&h.repo, "API_TOKEN")).await;
        outcome.assert_no_value();
        let answer = outcome.answer();
        assert_eq!(answer["reference"], "secret://API_TOKEN");
        assert_eq!(answer["key"], "API_TOKEN");
        assert_eq!(answer["present"], true);
        assert_eq!(answer["scope"], "project");
        assert_eq!(answer["vault"], PROJECT_VAULT);
        assert_eq!(answer["backend"], "keychain");
        assert!(answer["imported_at"].is_u64(), "{answer}");
        assert_eq!(answer.as_object().expect("object").len(), 7, "{answer}");
        // The capture works: the tool's own log line, by key name, is in it.
        assert!(
            outcome.logs.contains("secrets_get_ref answered") && outcome.logs.contains("API_TOKEN"),
            "the log capture missed the tool's line: {}",
            outcome.logs
        );
    }

    /// DOC-74 §15.3: `secret://KEY` falls back to the owner vault; an explicit
    /// owner reference names it directly.
    #[tokio::test]
    async fn secrets_get_ref_falls_back_to_the_owner_scope() {
        let h = harness().await;
        seed(&h, OWNER_VAULT, "SHARED_TOKEN").await;

        for (key, reference) in [
            ("SHARED_TOKEN", "secret://SHARED_TOKEN"),
            ("secret://acme/SHARED_TOKEN", "secret://acme/SHARED_TOKEN"),
        ] {
            let outcome = run_tool(&h.client, h.tmp.path(), args(&h.repo, key)).await;
            outcome.assert_no_value();
            let answer = outcome.answer();
            assert_eq!(answer["reference"], reference, "{answer}");
            assert_eq!(answer["present"], true, "{answer}");
            assert_eq!(answer["scope"], "owner", "{answer}");
            assert_eq!(answer["vault"], OWNER_VAULT, "{answer}");
        }
    }

    /// An unknown key is `present: false`, with no scope, vault or time.
    #[tokio::test]
    async fn secrets_get_ref_reports_an_unknown_key_as_absent() {
        let h = harness().await;
        seed(&h, PROJECT_VAULT, "API_TOKEN").await;

        let outcome = run_tool(&h.client, h.tmp.path(), args(&h.repo, "MISSING_KEY")).await;
        outcome.assert_no_value();
        let answer = outcome.answer();
        assert_eq!(answer["present"], false, "{answer}");
        assert_eq!(answer["key"], "MISSING_KEY");
        assert!(answer["scope"].is_null(), "{answer}");
        assert!(answer["vault"].is_null(), "{answer}");
        assert!(answer["imported_at"].is_null(), "{answer}");
    }

    /// A socket nothing serves, with no binary to start, is a fixed error.
    #[tokio::test]
    async fn secrets_get_ref_fails_without_a_value_when_the_socket_is_unreachable() {
        let h = harness().await;
        seed(&h, PROJECT_VAULT, "API_TOKEN").await;
        let dead = Arc::new(client_at(h.tmp.path(), &h.tmp.path().join("dead.sock")));

        let outcome = run_tool(&dead, h.tmp.path(), args(&h.repo, "API_TOKEN")).await;
        outcome.assert_no_value();
        assert!(outcome.is_error, "{}", outcome.text);
        assert!(
            outcome
                .text
                .starts_with("secrets_get_ref: cannot start trusty-secrets"),
            "{}",
            outcome.text
        );
    }

    /// A checkout with no remote has no scopes: the server's fixed refusal.
    #[tokio::test]
    async fn secrets_get_ref_fails_without_a_value_when_the_project_is_unresolved() {
        let h = harness().await;
        seed(&h, PROJECT_VAULT, "API_TOKEN").await;
        let bare = h.tmp.path().join("bare");
        std::fs::create_dir(&bare).expect("mkdir bare");
        git(&bare, &["init", "-q"]);

        let outcome = run_tool(&h.client, h.tmp.path(), args(&bare, "API_TOKEN")).await;
        outcome.assert_no_value();
        assert!(outcome.is_error, "{}", outcome.text);
        assert_eq!(
            outcome.text,
            "secrets_get_ref: secrets.scopes: cannot determine the project's scopes from its git remote or config"
        );
    }

    /// A relative project, a bad key and a bad reference fail before any call.
    #[tokio::test]
    async fn secrets_get_ref_refuses_a_relative_project_and_an_invalid_key() {
        let h = harness().await;
        seed(&h, PROJECT_VAULT, "API_TOKEN").await;
        let cases = [
            (
                json!({ "project": "repo", "key": "API_TOKEN" }),
                "`project` must be an absolute path",
            ),
            (args(&h.repo, "bad key"), "invalid secret key"),
            (
                args(&h.repo, "secret://a/b/c/d"),
                "invalid secret reference",
            ),
        ];
        for (arguments, expected) in cases {
            let outcome = run_tool(&h.client, h.tmp.path(), arguments).await;
            outcome.assert_no_value();
            assert!(outcome.is_error, "{}", outcome.text);
            assert!(outcome.text.contains(expected), "{}", outcome.text);
        }
    }
}
