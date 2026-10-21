//! Live checks of `tm secrets exec` against a real trusty-secrets server
//! (#9621). Every test is `#[ignore]`.
//!
//! Why: the fake socket in `tm_secrets_exec.rs` cannot judge process
//! ancestry or keep an audit trail. These tests run the real server
//! in-process, over the OS process table, so a grant registered by tm and a
//! resolve from the stub pass the same checks they pass in production.
//! What: [`Live`] serves trusty-secrets' own `serve` under a scratch `$HOME`
//! (`TRUSTY_SECRETS_EXTERNAL=1`, so tm spawns no server of its own). Each
//! test first asserts that its own parent chain holds no Claude Code process
//! ([`assert_detached`]): an agent ancestor would make the server refuse
//! unflagged keys, and the run would prove nothing. Run them detached, e.g.
//! `setsid -f` or `systemd-run --user --scope` on Linux, or
//! `launchctl asuser` on macOS.
//! Test: this file IS the test module.

use crate::common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use trusty_mpm::secrets_client::with_project;
use trusty_secrets::server::{
    BackendFactory, ClientError, ErrorKind, OnDemandSecrets, ServerSettings, serve,
};
use trusty_secrets::store::SecretBackend;
use trusty_secrets::{BackendId, SecretsError};

const VALUE: &str = "sk-live-9621-LIVE-c0ffee-SENTINEL";

/// Whether a process name or path names Claude Code: the server's rule.
fn names_agent(label: &str) -> bool {
    label.split('/').any(|segment| {
        let s = segment.to_ascii_lowercase();
        s == "claude" || s.starts_with("claude-") || s.starts_with("claude.")
    })
}

/// Fail unless no process above this one is Claude Code.
fn assert_detached() {
    let mut pid = std::process::id();
    while pid > 1 {
        let out = std::process::Command::new("ps")
            .args(["-o", "ppid=", "-o", "comm=", "-p", &pid.to_string()])
            .output()
            .expect("ps");
        let line = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        let (ppid, comm) = line.split_once(char::is_whitespace).expect("ps line");
        assert!(
            !names_agent(comm.trim()),
            "pid {pid} ({comm}) is Claude Code: run this test detached from the agent tree"
        );
        pid = ppid.trim().parse().expect("ppid");
    }
}

fn git(dir: &Path, args: &[&str]) {
    let ok = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .status()
        .expect("git");
    assert!(ok.success(), "git {args:?}");
}

/// A run's exit status (signal included), stdout and stderr, for a message.
// #9621: its only caller is the macOS Keychain proof.
#[cfg(target_os = "macos")]
fn report(out: &std::process::Output) -> String {
    format!(
        "status {}; stdout {:?}; stderr {:?}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// A real trusty-secrets server over `backend`, under a scratch `$HOME`.
///
/// #9072: shared with `tm_secrets_resolve`, which seeds flagged keys under
/// `index` and so needs no detached run.
pub(crate) struct Live {
    pub(crate) home: tempfile::TempDir,
    pub(crate) repo: PathBuf,
    pub(crate) vault: String,
    /// The server's names-index root.
    pub(crate) index: PathBuf,
    audit: PathBuf,
    client: OnDemandSecrets,
    _stop: tokio::sync::oneshot::Sender<()>,
}

impl Live {
    pub(crate) async fn new(backend: Arc<dyn SecretBackend>, repo_name: &str) -> Self {
        let home = tempfile::Builder::new()
            .prefix("tsl")
            .tempdir_in("/tmp")
            .expect("home");
        let repo = home.path().join("repo");
        std::fs::create_dir(&repo).expect("repo");
        git(&repo, &["init", "-q"]);
        let remote = format!("git@github.com:trusty-test/{repo_name}.git");
        git(&repo, &["remote", "add", "origin", &remote]);
        let socket = home
            .path()
            .join(".trusty-tools/trusty-secrets/secrets.sock");
        let settings = ServerSettings::new(
            socket.clone(),
            home.path().join("state/index"),
            home.path().join("machine.yaml"),
            Duration::from_secs(120),
        );
        std::fs::write(
            &settings.machine_config,
            "secrets:\n  default_backend: keychain\n",
        )
        .expect("machine.yaml");
        let audit = settings.audit_log.clone();
        let index = settings.index_root.clone();
        let factory: BackendFactory = Arc::new(move |id: &BackendId| match id.as_str() {
            BackendId::KEYCHAIN => Ok(Arc::clone(&backend)),
            other => Err(SecretsError::UnknownBackend {
                backend: other.to_owned(),
            }),
        });
        let (stop, rx) = tokio::sync::oneshot::channel::<()>();
        tokio::spawn(serve(settings, factory, async move {
            let _ = rx.await;
        }));
        let start = Instant::now();
        while !trusty_common::uds::socket_is_serving(&socket, Duration::from_millis(200)).await {
            assert!(
                start.elapsed() < Duration::from_secs(20),
                "socket never served"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let client = OnDemandSecrets::at(&socket).with_program(home.path().join("no-server"));
        Self {
            vault: format!("trusty/trusty-test/{}", repo_name.to_ascii_lowercase()),
            home,
            repo,
            index,
            audit,
            client,
            _stop: stop,
        }
    }

    /// One project-scoped call from this (detached) test process.
    pub(crate) async fn call(&self, method: &str, params: Value) -> Result<Value, ClientError> {
        let params = with_project(&self.repo, params).expect("utf-8");
        self.client.call(method, params).await
    }

    async fn set(&self, key: &str) {
        let params = json!({"vault": self.vault, "key": key, "value": VALUE});
        self.call("secrets.set", params).await.expect("secrets.set");
    }

    /// `tm secrets exec <args>` in the repo; `wrapper` runs it (a fake `claude`).
    async fn exec(&self, wrapper: Option<&Path>, args: &[&str]) -> std::process::Output {
        let mut cmd = match wrapper {
            Some(shell) => {
                let mut cmd = std::process::Command::new(shell);
                // A second command stops the shell exec'ing tm in its place.
                cmd.args(["-c", r#""$TM" secrets exec "$@"; exit $?"#, "claude"])
                    .args(args);
                common::isolate_spawned_tm(&mut cmd, self.home.path());
                cmd.env("TM", common::tm_bin());
                cmd
            }
            None => {
                let mut cmd = common::tm_command_in(self.home.path());
                cmd.args(["secrets", "exec"]).args(args);
                cmd
            }
        };
        cmd.current_dir(&self.repo)
            .env("TRUSTY_SECRETS_EXTERNAL", "1")
            .env("OUT", self.home.path().join("out"))
            .stdin(std::process::Stdio::null());
        tokio::process::Command::from(cmd)
            .output()
            .await
            .expect("tm output")
    }

    fn audit(&self) -> Vec<Value> {
        std::fs::read_to_string(&self.audit)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }
}

/// Why: #9621 end to end on a real server — the grant is registered for the
/// stub, the command resolves an unflagged key with no agent above it, and
/// after exit the grant is gone: a resolve with its token is refused.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "live: real process ancestry; run detached from any Claude Code tree"]
async fn live_exec_spawn_and_revoke() {
    assert_detached();
    let backend = Arc::new(trusty_secrets::store::MemoryBackend::new());
    let live = Live::new(backend, "exec-9621").await;
    live.set("GH_TOKEN").await;
    let script = r#"echo $$ > "$OUT"; cat <&"$TRUSTY_SECRETS_TOKEN_FD" > "$OUT.tok"; printf %s "$FOO" | wc -c"#;
    let out = live
        .exec(None, &["--env", "FOO=GH_TOKEN", "--", "sh", "-c", script])
        .await;
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        VALUE.len().to_string()
    );
    let command_pid: u64 = std::fs::read_to_string(live.home.path().join("out"))
        .expect("pid")
        .trim()
        .parse()
        .expect("pid");
    let token = std::fs::read_to_string(live.home.path().join("out.tok")).expect("token");

    let records = live.audit();
    let of = |method: &str| -> Vec<&Value> {
        records.iter().filter(|r| r["method"] == method).collect()
    };
    let resolves = of("secrets.resolve");
    assert_eq!(resolves.len(), 1, "one resolve per key: {records:?}");
    assert_eq!(resolves[0]["decision"], "allow");
    assert_eq!(
        resolves[0]["caller_pid"],
        json!(command_pid),
        "the stub resolved, as the granted pid"
    );
    assert_eq!(resolves[0]["agent_parent"], json!(false));
    assert_eq!(
        of("secrets.revoke").len(),
        1,
        "revoked on exit: {records:?}"
    );

    let resolve = json!({"token": token, "key": "GH_TOKEN"});
    match live.client.call("secrets.resolve", resolve).await {
        Err(ClientError::Rpc(failure)) => assert_eq!(failure.kind, Some(ErrorKind::GrantRefused)),
        other => panic!("a revoked token resolved: {}", other.is_ok()),
    }
}

/// Why: DOC-74 §15.8 on the real Keychain — a detached caller may turn the
/// "agents may use" flag ON (the file backend refuses ON, so only the
/// Keychain proves the write); under a `claude` parent the flagged key is
/// injected and an unflagged key is refused. The flag is set back OFF and
/// both keys removed before any assertion.
#[cfg(target_os = "macos")]
#[tokio::test(flavor = "multi_thread")]
#[ignore = "live: writes the login Keychain; run detached from any Claude Code tree"]
async fn live_keychain_flag_on_write_proof() {
    assert_detached();
    let backend = trusty_secrets::store::open_backend(&BackendId::keychain()).expect("keychain");
    let repo_name = format!("exec-9621-{}", std::process::id());
    let live = Live::new(backend, &repo_name).await;
    live.set("FLAGGED").await;
    live.set("PLAIN").await;
    let flag = |allowed: bool| json!({"vault": live.vault, "key": "FLAGGED", "allowed": allowed});
    let flag_on = live.call("secrets.set_agents_may_use", flag(true)).await;

    // A copy of bash named `claude`: the server reads its path and name.
    let bin = live.home.path().join("bin");
    std::fs::create_dir(&bin).expect("bin");
    let claude = bin.join("claude");
    std::fs::copy("/bin/bash", &claude).expect("copy bash");
    // #9621: macOS SIGKILLs a copy of a platform binary at exec; an ad-hoc
    // signature makes the copy an ordinary binary. No Keychain is involved.
    let signed = std::process::Command::new("/usr/bin/codesign")
        .args(["--force", "--sign", "-"])
        .arg(&claude)
        .output()
        .expect("codesign");
    assert!(signed.status.success(), "codesign: {}", report(&signed));
    let script = r#"printf %s "$A" | wc -c"#;
    let flagged = live
        .exec(
            Some(&claude),
            &["--env", "A=FLAGGED", "--", "sh", "-c", script],
        )
        .await;
    let plain = live
        .exec(
            Some(&claude),
            &["--env", "A=PLAIN", "--", "sh", "-c", script],
        )
        .await;

    let flag_off = live.call("secrets.set_agents_may_use", flag(false)).await;
    for key in ["FLAGGED", "PLAIN"] {
        let _ = live
            .call("secrets.delete", json!({"vault": live.vault, "key": key}))
            .await;
    }

    assert!(
        flag_on.is_ok(),
        "a detached caller could not turn the flag on"
    );
    assert!(flag_off.is_ok(), "the flag was not set back OFF");
    // #9621: a run that dies on a signal prints nothing; name its status.
    assert!(flagged.status.success(), "flagged: {}", report(&flagged));
    assert_eq!(
        String::from_utf8_lossy(&flagged.stdout).trim(),
        VALUE.len().to_string()
    );
    let refused = String::from_utf8_lossy(&plain.stderr);
    assert_eq!(plain.status.code(), Some(1), "plain: {}", report(&plain));
    assert!(refused.contains("nothing ran"), "{refused}");
    let records = live.audit();
    assert!(
        records
            .iter()
            .any(|r| r["method"] == "secrets.grant" && r["agent_parent"] == json!(true)),
        "no grant was judged under the fake claude: {records:?}"
    );
}
