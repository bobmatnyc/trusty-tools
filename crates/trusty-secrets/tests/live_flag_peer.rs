//! Live checks of the agent rule and the kernel peer pid, against the real
//! `trusty-secrets` binary and the host's real process table (#9070 S8).
//!
//! Why: the socket tests judge ancestry on a fake table. These two harnesses
//! prove the same rules on the host's own process tree, with no fake and no
//! env marker:
//! - [`live_claude_named_ancestor_is_refused_a_grant`] (for EVO): a copy of
//!   this test binary named `claude` is the real parent of the registrar, so
//!   the kernel's parent chain and the detector are real; only the program
//!   is not Anthropic's. A copy named `notclaude` is the control. Run it on a
//!   host that is not itself under Claude Code, or the control is refused.
//! - [`live_peer_pid_is_the_kernels`] (for the Mac, `LOCAL_PEERPID`; it runs
//!   on Linux too, `SO_PEERCRED`): a detached registrar grants a child; the
//!   child and its grandchild resolve, a sibling holding the valid token is
//!   refused, and each audit record's `caller_pid` is its caller's real pid.
//!   The registrar detaches (re-parents away from the test), so it runs
//!   outside a Claude Code tree on the Mac.
//!
//! What: each test re-runs this binary in roles chosen by [`ROLE`]. Every
//! path is under one short temp directory in `/private/tmp` (macOS) or
//! `/tmp`, so the socket path fits `sun_path` (104 bytes on macOS). `HOME`
//! points there and the machine config selects the `file` backend, so no
//! Keychain item, no real home and no `.env.local` is touched. Values and
//! tokens are never printed; a role reports pids, codes and booleans only.
//! Test: itself (both `#[ignore]`; run with `--ignored --nocapture`).

#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tempfile::TempDir;
use trusty_common::uds::server::RpcResponse;
use trusty_common::uds::{send_framed_request, socket_is_serving};
use trusty_secrets::store::{FileBackend, NamesIndex, SecretBackend, SecretStore, VALUES_SUBDIR};
use trusty_secrets::{SecretKey, SecretValue, VaultName};

const BIN: &str = env!("CARGO_BIN_EXE_trusty-secrets");
/// The env var naming a re-run's role; unset in the orchestrator.
const ROLE: &str = "TS_LIVE_ROLE";
const SOCKET: &str = "TS_LIVE_SOCKET";
const PROJECT: &str = "TS_LIVE_PROJECT";
const OUT: &str = "TS_LIVE_OUT";
const TOKEN: &str = "TS_LIVE_TOKEN";
const ORIGINAL: &str = "TS_LIVE_ORIGINAL";
const VAULT: &str = "trusty/acme/web";
const KEY: &str = "API_KEY";
/// The seeded value; compared, never printed.
const VALUE: &str = "live-9070-value-Qm4Tz8";
const GRANT_TEST: &str = "live_claude_named_ancestor_is_refused_a_grant";
const PEER_TEST: &str = "live_peer_pid_is_the_kernels";

/// One temp root with every path the server and the roles use.
struct Live {
    root: TempDir,
    server: Option<Child>,
}

impl Live {
    fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    fn socket(&self) -> PathBuf {
        self.path("run").join("s.sock")
    }

    fn audit_log(&self) -> PathBuf {
        self.path("audit").join("audit.jsonl")
    }

    /// Every audit record, in order.
    fn records(&self) -> Vec<Value> {
        let text = std::fs::read_to_string(self.audit_log()).unwrap_or_default();
        assert!(!text.contains(VALUE), "a value reached the audit log");
        text.lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        if let Some(mut server) = self.server.take() {
            let _ = server.kill();
            let _ = server.wait();
        }
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

/// One request over `socket`, from this process.
fn rpc(socket: &Path, method: &str, params: Value) -> RpcResponse {
    let request = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
    runtime()
        .block_on(send_framed_request(
            socket,
            &request,
            Duration::from_secs(30),
        ))
        .unwrap()
}

/// A short temp root, a checkout with a github remote, `API_KEY` seeded in
/// the file backend under the temp `HOME`, and the real binary serving.
fn live() -> Live {
    let base = if cfg!(target_os = "macos") {
        "/private/tmp"
    } else {
        "/tmp"
    };
    let root = tempfile::Builder::new()
        .prefix("ts")
        .tempdir_in(base)
        .unwrap();
    let mut live = Live { root, server: None };
    let repo = live.path("repo");
    std::fs::create_dir(&repo).unwrap();
    for args in [
        vec!["init", "-q"],
        vec!["remote", "add", "origin", "git@github.com:Acme/Web.git"],
    ] {
        let ok = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(&args)
            .status();
        assert!(ok.unwrap().success(), "git {args:?}");
    }
    std::fs::write(
        live.path("machine.yaml"),
        "secrets:\n  default_backend: file\n",
    )
    .unwrap();
    let home = live.path("home");
    std::fs::create_dir(&home).unwrap();
    let backend: Arc<dyn SecretBackend> = Arc::new(FileBackend::at(home.join(VALUES_SUBDIR)));
    SecretStore::new(backend, NamesIndex::at(live.path("index")))
        .set(
            &VaultName::new(VAULT).unwrap(),
            &SecretKey::new(KEY).unwrap(),
            &SecretValue::new(VALUE),
        )
        .unwrap();
    let server = Command::new(BIN)
        .args(["serve", "--socket"])
        .arg(live.socket())
        .arg("--index-dir")
        .arg(live.path("index"))
        .arg("--machine-config")
        .arg(live.path("machine.yaml"))
        .arg("--audit-log")
        .arg(live.audit_log())
        .args(["--idle-timeout-secs", "120"])
        .env("HOME", &home)
        .env_remove("TRUSTY_SECRETS_SOCKET")
        .env_remove("TRUSTY_SECRETS_INDEX_DIR")
        .env_remove("TRUSTY_SECRETS_IDLE_TIMEOUT_SECS")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    live.server = Some(server);
    let started = Instant::now();
    while !runtime().block_on(socket_is_serving(
        &live.socket(),
        Duration::from_millis(200),
    )) {
        assert!(started.elapsed() < Duration::from_secs(20), "never served");
        std::thread::sleep(Duration::from_millis(50));
    }
    live
}

/// Re-run this binary's `test` in `role`, from `exe`.
fn role(exe: &Path, test: &str, role: &str) -> Command {
    let mut command = Command::new(exe);
    command
        .args([
            test,
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(ROLE, role)
        .env_remove(TOKEN);
    command
}

/// The `(code, kind)` of an error reply; `None` on success.
fn error_of(response: &RpcResponse) -> Option<(i64, String)> {
    response.error.as_ref().map(|e| {
        let kind = e.data.as_ref().and_then(|d| d["kind"].as_str());
        (i64::from(e.code), kind.unwrap_or_default().to_owned())
    })
}

/// Write a role's report as one JSON file under `OUT`.
fn report(name: &str, body: Value) {
    let dir = PathBuf::from(std::env::var(OUT).unwrap());
    std::fs::write(dir.join(format!("{name}.json")), body.to_string()).unwrap();
}

fn read_report(dir: &Path, name: &str) -> Value {
    let text = std::fs::read_to_string(dir.join(format!("{name}.json")))
        .unwrap_or_else(|e| panic!("no {name} report: {e}"));
    serde_json::from_str(&text).unwrap()
}

/// `secrets.resolve` of [`KEY`] with `token`; the report.
fn resolve_report(token: &str) -> Value {
    let socket = PathBuf::from(std::env::var(SOCKET).unwrap());
    let response = rpc(
        &socket,
        "secrets.resolve",
        json!({"token": token, "key": KEY}),
    );
    let value_ok = response
        .result
        .as_ref()
        .is_some_and(|r| r["value"].as_str() == Some(VALUE));
    json!({"pid": std::process::id(), "error": error_of(&response), "value_ok": value_ok})
}

/// `secrets.grant` of [`KEY`] for `child`; the error, or the token.
fn grant(child: u32) -> Result<String, (i64, String)> {
    let socket = PathBuf::from(std::env::var(SOCKET).unwrap());
    let project = std::env::var(PROJECT).unwrap();
    let params = json!({"project": project, "child_pid": child, "keys": [KEY], "ttl_secs": 120});
    let response = rpc(&socket, "secrets.grant", params);
    match error_of(&response) {
        Some(error) => Err(error),
        None => Ok(response.result.unwrap()["token"]
            .as_str()
            .unwrap()
            .to_owned()),
    }
}

fn parent_pid() -> u32 {
    // SAFETY: `getppid` takes no arguments and cannot fail.
    let ppid = unsafe { libc::getppid() };
    u32::try_from(ppid).unwrap()
}

/// Why: DOC-74 §15.8, the `agent_parent=true` refusal on a real process
/// tree — a registrar whose parent is named `claude` is refused an unflagged
/// key with `agent_use_refused` (-32065); one named `notclaude` is granted.
/// Test: itself.
#[test]
#[ignore = "live: spawns the real binary and copies of this test binary; run on EVO"]
fn live_claude_named_ancestor_is_refused_a_grant() {
    match std::env::var(ROLE).as_deref() {
        // The copy: spawn the registrar from the original binary, so this
        // copy is its real parent.
        Ok("launcher") => {
            let original = PathBuf::from(std::env::var(ORIGINAL).unwrap());
            let status = role(&original, GRANT_TEST, "granter").status().unwrap();
            assert!(status.success());
            return;
        }
        Ok("granter") => {
            let outcome = grant(std::process::id());
            let launcher = std::env::var("TS_LIVE_LAUNCHER").unwrap();
            report(
                &launcher,
                json!({"pid": std::process::id(), "ppid": parent_pid(),
                    "error": outcome.as_ref().err(), "granted": outcome.is_ok()}),
            );
            return;
        }
        _ => {}
    }
    let live = live();
    let exe = std::env::current_exe().unwrap();
    let bin = live.path("bin");
    std::fs::create_dir(&bin).unwrap();
    let out = live.path("out");
    std::fs::create_dir(&out).unwrap();
    for name in ["claude", "notclaude"] {
        let copy = bin.join(name);
        std::fs::copy(&exe, &copy).unwrap();
        let mut launcher = role(&copy, GRANT_TEST, "launcher")
            .env(ORIGINAL, &exe)
            .env("TS_LIVE_LAUNCHER", name)
            .env(SOCKET, live.socket())
            .env(PROJECT, live.path("repo"))
            .env(OUT, &out)
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let launcher_pid = launcher.id();
        assert!(launcher.wait().unwrap().success(), "{name} launcher");
        let got = read_report(&out, name);
        assert_eq!(
            got["ppid"], launcher_pid,
            "{name} is the registrar's parent"
        );
        println!("{name}: {got}");
    }
    let claude = read_report(&out, "claude");
    assert_eq!(claude["error"], json!([-32065, "agent_use_refused"]));
    assert_eq!(read_report(&out, "notclaude")["granted"], true);

    let grants: Vec<Value> = live
        .records()
        .into_iter()
        .filter(|r| r["method"] == "secrets.grant")
        .collect();
    assert_eq!(grants.len(), 2, "{grants:?}");
    assert_eq!(grants[0]["decision"], "deny");
    assert_eq!(grants[0]["reason"], "agent_use_refused");
    assert_eq!(grants[0]["key"], KEY);
    assert_eq!(grants[0]["agent_parent"], true);
    assert_eq!(grants[0]["caller_pid"], claude["pid"]);
    assert_eq!(grants[1]["decision"], "allow");
    assert_eq!(grants[1]["agent_parent"], false);
}

/// Why: DOC-74 §15.8 condition 3 on the host's own peer-pid read — only
/// the granted child and its descendants resolve, and the pid in each audit
/// record is the kernel's, not one the caller sent.
/// Test: itself.
#[test]
#[ignore = "live: spawns the real binary and a detached process tree; run on the Mac"]
fn live_peer_pid_is_the_kernels() {
    let me = || std::env::current_exe().unwrap();
    match std::env::var(ROLE).as_deref() {
        // Start the registrar and exit at once, so it re-parents away from
        // this test (and from any Claude Code above it).
        Ok("detach") => {
            role(&me(), PEER_TEST, "registrar")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            return;
        }
        Ok("registrar") => {
            let first = parent_pid();
            let started = Instant::now();
            while parent_pid() == first && started.elapsed() < Duration::from_secs(10) {
                std::thread::sleep(Duration::from_millis(20));
            }
            let mut child = role(&me(), PEER_TEST, "child")
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .spawn()
                .unwrap();
            let outcome = grant(child.id());
            let token = outcome.clone().unwrap_or_default();
            let mut stdin = child.stdin.take().unwrap();
            writeln!(stdin, "{token}").unwrap();
            drop(stdin);
            let sibling = role(&me(), PEER_TEST, "sibling")
                .env(TOKEN, &token)
                .stdout(Stdio::null())
                .status()
                .unwrap();
            let child_ok = child.wait().unwrap().success();
            report(
                "registrar",
                json!({"pid": std::process::id(), "ppid": parent_pid(), "first_ppid": first,
                    "child": child.id(), "error": outcome.err(),
                    "child_ok": child_ok, "sibling_ok": sibling.success()}),
            );
            return;
        }
        Ok("child") => {
            let mut token = String::new();
            BufReader::new(std::io::stdin())
                .read_line(&mut token)
                .unwrap();
            let token = token.trim();
            report("child", resolve_report(token));
            let status = role(&me(), PEER_TEST, "grandchild")
                .env(TOKEN, token)
                .stdout(Stdio::null())
                .status()
                .unwrap();
            assert!(status.success());
            return;
        }
        Ok(name @ ("sibling" | "grandchild")) => {
            let mut got = resolve_report(&std::env::var(TOKEN).unwrap());
            got["ppid"] = json!(parent_pid());
            report(name, got);
            return;
        }
        _ => {}
    }
    let live = live();
    let out = live.path("out");
    std::fs::create_dir(&out).unwrap();
    let status = role(&me(), PEER_TEST, "detach")
        .env(SOCKET, live.socket())
        .env(PROJECT, live.path("repo"))
        .env(OUT, &out)
        .status()
        .unwrap();
    assert!(status.success(), "detach");
    let started = Instant::now();
    while !out.join("registrar.json").exists() {
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "the registrar never reported"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    let registrar = read_report(&out, "registrar");
    let (child, grandchild, sibling) = (
        read_report(&out, "child"),
        read_report(&out, "grandchild"),
        read_report(&out, "sibling"),
    );
    println!("registrar {registrar}\nchild {child}\ngrandchild {grandchild}\nsibling {sibling}");
    assert_ne!(
        registrar["ppid"], registrar["first_ppid"],
        "the registrar detached"
    );
    assert_eq!(registrar["error"], Value::Null, "the grant was minted");
    assert_eq!(child["pid"], registrar["child"]);
    assert_eq!(grandchild["ppid"], child["pid"]);
    assert_eq!(child["value_ok"], true);
    assert_eq!(grandchild["value_ok"], true);
    assert_eq!(sibling["value_ok"], false);
    assert_eq!(sibling["error"], json!([-32082, "grant_refused"]));

    let resolves: Vec<(Value, Value)> = live
        .records()
        .into_iter()
        .filter(|r| r["method"] == "secrets.resolve")
        .map(|r| (r["caller_pid"].clone(), r["decision"].clone()))
        .collect();
    let expected = [
        (child["pid"].clone(), json!("allow")),
        (grandchild["pid"].clone(), json!("allow")),
        (sibling["pid"].clone(), json!("deny")),
    ];
    for record in &expected {
        assert!(resolves.contains(record), "{record:?} not in {resolves:?}");
    }
    assert_eq!(resolves.len(), 3, "{resolves:?}");
    let grants: Vec<Value> = live
        .records()
        .into_iter()
        .filter(|r| r["method"] == "secrets.grant")
        .collect();
    assert_eq!(grants.len(), 1, "{grants:?}");
    assert_eq!(grants[0]["caller_pid"], registrar["pid"]);
    assert_eq!(grants[0]["agent_parent"], false);
}
