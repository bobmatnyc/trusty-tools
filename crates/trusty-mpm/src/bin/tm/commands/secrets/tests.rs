//! Tests for `tm secrets` against an in-process trusty-secrets server (#7521).
//!
//! No test touches the OS Keychain, the real clipboard, or `~/.trusty-tools`:
//! the server serves a socket in a `TempDir` over a `MemoryBackend` as
//! `keychain`, its index and machine config sit in the same
//! `TempDir`, every value source is a fixed string, and the client's spawn
//! program is a path that does not exist, so no real `trusty-secrets` runs.
//! Every run captures stdout, the error's `Display` and `Debug`, and TRACE
//! tracing, and [`Outcome::assert_no_value`] checks all four.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use clap::Parser;
use tempfile::TempDir;
use tokio::sync::oneshot;
use trusty_secrets::server::{BackendFactory, OnDemandSecrets, ServerSettings, serve};
use trusty_secrets::store::{MemoryBackend, SecretBackend};
use trusty_secrets::{BackendId, SecretKey, SecretsError, VaultName};

use super::*;
use crate::cli::{Cli, Command};

/// A value longer than 8 characters: only [`HEAD`] may ever be printed.
const VALUE: &str = "sk-live-TAIL-7521-c0ffee";
/// The first 8 characters of [`VALUE`].
const HEAD: &str = "sk-live-";
/// Everything after [`HEAD`]; no output may carry it.
const TAIL: &str = "TAIL-7521-c0ffee";
/// A value of exactly 8 characters: no character of it may be printed.
const SHORT: &str = "Q7xZ-9wK";
/// The project vault of the fixture's `git@github.com:Acme/Web.git`.
const PROJECT_VAULT: &str = "trusty/acme/web";

/// A value source that returns one fixed text.
struct Fixed(&'static str);

impl ValueSource for Fixed {
    fn read(&self) -> anyhow::Result<String> {
        Ok(self.0.to_owned())
    }
}

/// A served socket over a memory backend, in a temp dir.
struct Harness {
    tmp: TempDir,
    repo: PathBuf,
    keychain: Arc<MemoryBackend>,
    client: OnDemandSecrets,
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
    let settings = ServerSettings {
        socket: tmp.path().join("run").join("s.sock"),
        index_root: tmp.path().join("index"),
        machine_config: tmp.path().join("machine.yaml"),
        idle_timeout: Duration::from_secs(60),
    };
    let keychain = Arc::new(MemoryBackend::new());
    let kc = Arc::clone(&keychain);
    let backends: BackendFactory = Arc::new(move |id: &BackendId| {
        if id.as_str() != BackendId::KEYCHAIN {
            return Err(SecretsError::UnknownBackend {
                backend: id.to_string(),
            });
        }
        Ok(Arc::clone(&kc) as Arc<dyn SecretBackend>)
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
    let client = client_at(tmp.path(), &socket);
    Harness {
        tmp,
        repo,
        keychain,
        client,
        _shutdown: tx,
    }
}

/// Collects every tracing line written while a verb runs.
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

/// What one verb produced: stdout, the error (`{:#}` and `{:?}`), and logs.
struct Outcome {
    out: String,
    err: Option<String>,
    logs: String,
}

impl Outcome {
    /// No text carries [`TAIL`] or any of [`SHORT`].
    fn assert_no_value(&self) {
        let err = self.err.as_deref().unwrap_or("");
        for (what, text) in [("stdout", &*self.out), ("error", err), ("logs", &self.logs)] {
            assert!(!text.contains(TAIL), "{what} carries the value: {text}");
            assert!(
                !text.contains(&SHORT[..4]),
                "{what} carries the short value: {text}"
            );
        }
    }

    fn err(&self) -> &str {
        self.err.as_deref().expect("the verb failed")
    }
}

/// Parse `tm secrets <args>` and run it in `project` against `client`.
async fn run_in(
    client: &OnDemandSecrets,
    project: &Path,
    clipboard: &'static str,
    stdin: &'static str,
    args: &[&str],
) -> Outcome {
    let argv = ["tm", "secrets"].into_iter().chain(args.iter().copied());
    let Some(Command::Secrets { action }) = Cli::try_parse_from(argv).expect("parse").command
    else {
        panic!("not a secrets command");
    };
    let sink = LogSink::default();
    let writer = sink.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);
    let ctx = Ctx {
        client,
        project,
        clipboard: &Fixed(clipboard),
        stdin: &Fixed(stdin),
    };
    let mut out = Vec::new();
    let result = dispatch(&ctx, action, &mut out).await;
    out.flush().expect("flush");
    let logs = String::from_utf8_lossy(&sink.0.lock().expect("log sink")).into_owned();
    let outcome = Outcome {
        out: String::from_utf8(out).expect("utf-8 stdout"),
        err: result.err().map(|e| format!("{e:#}\n{e:?}")),
        logs,
    };
    outcome.assert_no_value();
    outcome
}

async fn run(h: &Harness, clipboard: &'static str, args: &[&str]) -> Outcome {
    run_in(&h.client, &h.repo, clipboard, "", args).await
}

fn stored(backend: &MemoryBackend, vault: &str, key: &str) -> Option<String> {
    let vault = VaultName::new(vault).expect("vault");
    let key = SecretKey::new(key).expect("key");
    backend
        .get(&vault, &key)
        .expect("memory get")
        .map(|v| v.expose().to_owned())
}

#[test]
fn cli_parses_every_secrets_verb() {
    for args in [
        vec!["set", "K"],
        vec!["set", "K", "grp", "--value", "-"],
        vec!["list"],
        vec!["doctor"],
    ] {
        let argv = ["tm", "secrets"].into_iter().chain(args.iter().copied());
        let cli = Cli::try_parse_from(argv).unwrap_or_else(|e| panic!("{args:?}: {e}"));
        assert!(
            matches!(cli.command, Some(Command::Secrets { .. })),
            "{args:?}"
        );
    }
    // `set` needs a key.
    assert!(Cli::try_parse_from(["tm", "secrets", "set"]).is_err());
    // #7521: the owner scope is the console's (DOC-74 §15.3).
    assert!(Cli::try_parse_from(["tm", "secrets", "set", "K", "--owner"]).is_err());
}

#[test]
fn parsed_set_arguments_never_show_in_debug() {
    let key = format!("API_KEY:{VALUE}");
    let argv = ["tm", "secrets", "set", &key, VALUE, "--value", VALUE, VALUE];
    let cli = Cli::try_parse_from(argv).expect("parse");
    let debug = format!("{cli:?}");
    assert!(debug.contains("Secrets"), "{debug}");
    assert!(!debug.contains(TAIL), "Debug shows the value: {debug}");
}

#[tokio::test]
async fn set_reads_the_clipboard_and_confirms_head_and_length_only() {
    let h = harness().await;
    let first = run(&h, " sk-live-TAIL-7521-c0ffee\n", &["set", "API_KEY"]).await;
    assert_eq!(first.err, None);
    assert_eq!(
        first.out,
        format!("(new) secret set API_KEY: {HEAD}… [24 chars]\n")
    );
    assert_eq!(
        stored(&h.keychain, PROJECT_VAULT, "API_KEY").as_deref(),
        Some(VALUE)
    );

    let second = run(&h, "sk-live-TAIL-7521-c0ffee-v2", &["set", "API_KEY"]).await;
    assert_eq!(
        second.out,
        format!("(updated) secret set API_KEY: {HEAD}… [27 chars]\n")
    );
}

#[tokio::test]
async fn set_with_a_short_value_shows_only_its_length() {
    let h = harness().await;
    let outcome = run(&h, SHORT, &["set", "PIN"]).await;
    assert_eq!(outcome.out, "(new) secret set PIN: [8 chars]\n");
    assert_eq!(
        stored(&h.keychain, PROJECT_VAULT, "PIN").as_deref(),
        Some(SHORT)
    );
}

#[tokio::test]
async fn set_with_an_empty_clipboard_is_an_error_and_stores_nothing() {
    let h = harness().await;
    let outcome = run(&h, " \n\t", &["set", "API_KEY"]).await;
    assert!(
        outcome.err().contains("the clipboard is empty"),
        "{}",
        outcome.err()
    );
    assert_eq!(outcome.out, "");
    let piped = run(&h, VALUE, &["set", "API_KEY", "--value", "-"]).await;
    assert!(piped.err().contains("stdin is empty"), "{}", piped.err());
    assert!(h.keychain.is_empty());
}

#[tokio::test]
async fn set_refuses_a_value_on_the_command_line_without_echoing_it() {
    let h = harness().await;
    let in_key = format!("API_KEY:{VALUE}");
    for args in [
        vec!["set", "API_KEY", "grp", VALUE],
        vec!["set", "API_KEY", "--value", VALUE],
        vec!["set", in_key.as_str()],
        vec!["set", "API_KEY", VALUE],
    ] {
        let outcome = run(&h, VALUE, &args).await;
        assert!(outcome.err.is_some(), "{args:?} was accepted");
        assert_eq!(outcome.out, "", "{args:?}");
    }
    assert!(h.keychain.is_empty());
}

#[tokio::test]
async fn set_with_a_group_namespaces_the_key() {
    let h = harness().await;
    let outcome = run_in(
        &h.client,
        &h.repo,
        "",
        VALUE,
        &["set", "API_KEY", "stripe", "--value", "-"],
    )
    .await;
    assert_eq!(
        outcome.out,
        format!("(new) secret set stripe.API_KEY: {HEAD}… [24 chars]\n")
    );
    let again = run(&h, VALUE, &["set", "API_KEY", "stripe"]).await;
    assert!(
        again
            .out
            .starts_with("(updated) secret set stripe.API_KEY: "),
        "{}",
        again.out
    );
    assert_eq!(
        stored(&h.keychain, PROJECT_VAULT, "stripe.API_KEY").as_deref(),
        Some(VALUE)
    );
    assert_eq!(stored(&h.keychain, PROJECT_VAULT, "API_KEY"), None);
    let dotted = run(&h, VALUE, &["set", "API_KEY", "a.b"]).await;
    assert!(
        dotted.err().contains("a group is 1 to 16"),
        "{}",
        dotted.err()
    );
}

#[tokio::test]
async fn list_prints_key_names_only() {
    let h = harness().await;
    run(&h, VALUE, &["set", "A_KEY"]).await;
    // The CLI writes the project scope only; seed the owner scope directly.
    let ctx = Ctx {
        client: &h.client,
        project: &h.repo,
        clipboard: &Fixed(""),
        stdin: &Fixed(""),
    };
    let params = serde_json::json!({ "vault": "trusty/acme", "key": "B_KEY", "value": SHORT });
    let _: serde_json::Value = ctx
        .call(trusty_secrets::api::methods::method::SET, params)
        .await
        .expect("owner-scope set");
    let outcome = run(&h, "", &["list"]).await;
    assert_eq!(
        outcome.out,
        "trusty/acme/web (Project):\n  A_KEY\ntrusty/acme (Owner):\n  B_KEY\n"
    );
    assert!(!outcome.out.contains(HEAD) && !outcome.out.contains("chars"));
}

#[tokio::test]
async fn doctor_reports_socket_and_backends_without_values() {
    let h = harness().await;
    run(&h, VALUE, &["set", "API_KEY"]).await;
    let outcome = run(&h, "", &["doctor"]).await;
    assert_eq!(outcome.err, None);
    let socket = h.client.socket().display().to_string();
    assert!(
        outcome.out.contains(&format!("socket {socket}: reachable")),
        "{}",
        outcome.out
    );
    assert!(
        outcome.out.contains("selected backend: keychain"),
        "{}",
        outcome.out
    );
    assert!(
        outcome
            .out
            .contains("backend keychain: available [READ, WRITE"),
        "{}",
        outcome.out
    );
    assert!(!outcome.out.contains(HEAD));

    // Outside a checkout: the project is reported, the socket still answers.
    let outside = h.tmp.path().join("plain");
    std::fs::create_dir(&outside).expect("mkdir");
    let bare = run_in(&h.client, &outside, "", "", &["doctor"]).await;
    assert_eq!(bare.err, None);
    assert!(
        bare.out
            .starts_with("project: tm secrets: secrets.doctor: "),
        "{}",
        bare.out
    );
    assert!(bare.out.contains(": reachable"), "{}", bare.out);
}

/// #7521: a server that refuses `secrets.doctor` answered, so the socket is
/// reachable; the refusal is the error.
#[tokio::test]
async fn doctor_reports_a_refusing_server_as_reachable() {
    let h = harness().await;
    std::fs::write(
        h.tmp.path().join("machine.yaml"),
        "secrets:\n  default_backend:\n    - not-a-backend-id\n",
    )
    .expect("write machine.yaml");
    let outcome = run(&h, "", &["doctor"]).await;
    let socket = h.client.socket().display().to_string();
    assert_eq!(outcome.out, format!("socket {socket}: reachable\n"));
    assert!(
        outcome.err().contains("config section is invalid"),
        "{}",
        outcome.err()
    );
}

/// #7521: a configured backend this build cannot open fails doctor after
/// the table.
#[tokio::test]
async fn doctor_fails_when_the_selected_backend_is_unavailable() {
    let h = harness().await;
    std::fs::write(
        h.tmp.path().join("machine.yaml"),
        "secrets:\n  default_backend: onepassword\n",
    )
    .expect("write machine.yaml");
    let outcome = run(&h, "", &["doctor"]).await;
    assert!(
        outcome.out.contains("selected backend: onepassword\n"),
        "{}",
        outcome.out
    );
    assert!(
        outcome.out.contains("backend keychain: available ["),
        "{}",
        outcome.out
    );
    assert!(
        outcome.out.ends_with("backend onepassword: unavailable\n"),
        "{}",
        outcome.out
    );
    assert!(
        outcome
            .err()
            .contains("the selected backend `onepassword` is unavailable"),
        "{}",
        outcome.err()
    );
}

#[tokio::test]
async fn every_verb_fails_without_a_value_when_the_socket_is_unreachable() {
    let h = harness().await;
    let dead = client_at(h.tmp.path(), &h.tmp.path().join("dead").join("s.sock"));
    for args in [
        vec!["set", "API_KEY"],
        vec!["set", "PIN", "--value", "-"],
        vec!["list"],
        vec!["doctor"],
    ] {
        let outcome = run_in(&dead, &h.repo, VALUE, SHORT, &args).await;
        assert!(
            outcome.err().contains("cannot start trusty-secrets"),
            "{args:?}: {}",
            outcome.err()
        );
    }
    let doctor = run_in(&dead, &h.repo, "", "", &["doctor"]).await;
    assert!(doctor.out.ends_with(": unreachable\n"), "{}", doctor.out);
    assert!(h.keychain.is_empty());
}
