//! The tier-3 client against the real `trusty-secrets` binary (#9071).
//!
//! Why: AC1 asks for a resolve "against a running service". The unit and
//! socket tests use fakes or an in-process server on a fake process table;
//! this one runs the real binary, the kernel's peer pid and the host's own
//! process tree, and the fd hand-down `tm secrets exec` uses (AC11).
//! What: the test re-runs this binary in roles chosen by [`ROLE`]. The
//! orchestrator starts the real server on `$HOME`'s default socket under a
//! temp home, with the `file` backend, and seeds `API_KEY` (the `file`
//! backend keeps no "agents may use" flag, so run it on a host that is not
//! under an agent, as EVO is; under Claude Code the grant is refused with
//! `agent_use_refused`). It spawns the `child` role holding a pipe on an inherited fd named by
//! `TRUSTY_SECRETS_TOKEN_FD`, grants the child's pid, writes the token,
//! closes the pipe and tells the child to go. The child calls
//! `trusty_secrets::granted::resolve` twice, then spawns the `grandchild`
//! role, which resolves through the token the child put back on the same fd.
//! Every path is under one short temp root, so no Keychain item, no real
//! home and no `.env.local` is touched. A role reports pids and booleans
//! only, never a token or a value.
//! Test: itself (`#[ignore]`; run with `--ignored`).

#![cfg(unix)]

use std::io::{BufRead, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tempfile::TempDir;
use trusty_common::uds::send_framed_request;
use trusty_common::uds::server::RpcResponse;
use trusty_secrets::api::TOKEN_FD_ENV;
use trusty_secrets::granted::{self, GrantedError};
// The server's own default path, so the client's default is what is tested.
use trusty_secrets::server::SOCKET_SUBPATH;
use trusty_secrets::store::{FileBackend, NamesIndex, SecretBackend, SecretStore, VALUES_SUBDIR};
use trusty_secrets::{SecretKey, SecretValue, VaultName};

const BIN: &str = env!("CARGO_BIN_EXE_trusty-secrets");
const TEST: &str = "granted_client_resolves_through_the_real_binary";
/// The env var naming a re-run's role; unset in the orchestrator.
const ROLE: &str = "TS_GRANTED_ROLE";
const OUT: &str = "TS_GRANTED_OUT";
const VAULT: &str = "trusty/acme/web";
const KEY: &str = "API_KEY";
/// The seeded value; compared, never printed.
const VALUE: &str = "live-9071-value-Hn5Rq2";

/// The temp root, and the server it started.
struct Live {
    root: TempDir,
    server: Option<Child>,
}

impl Live {
    fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    fn home(&self) -> PathBuf {
        self.path("home")
    }

    fn socket(&self) -> PathBuf {
        self.home().join(SOCKET_SUBPATH)
    }

    fn records(&self) -> Vec<Value> {
        let text =
            std::fs::read_to_string(self.path("audit").join("audit.jsonl")).unwrap_or_default();
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

/// A short temp root, a checkout with a github remote, `API_KEY` seeded in
/// the file backend under the temp `HOME`, and the real binary
/// serving `$HOME`'s default socket.
fn live() -> Live {
    let base = if cfg!(target_os = "macos") {
        "/private/tmp"
    } else {
        "/tmp"
    };
    let root = tempfile::Builder::new()
        .prefix("tg")
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
    std::fs::create_dir(live.home()).unwrap();
    let backend: Arc<dyn SecretBackend> =
        Arc::new(FileBackend::at(live.home().join(VALUES_SUBDIR)));
    let store = SecretStore::new(backend, NamesIndex::at(live.path("index")));
    let (vault, key) = (VaultName::new(VAULT).unwrap(), SecretKey::new(KEY).unwrap());
    store.set(&vault, &key, &SecretValue::new(VALUE)).unwrap();
    let server = Command::new(BIN)
        .args(["serve", "--socket"])
        .arg(live.socket())
        .arg("--index-dir")
        .arg(live.path("index"))
        .arg("--machine-config")
        .arg(live.path("machine.yaml"))
        .arg("--audit-log")
        .arg(live.path("audit").join("audit.jsonl"))
        .args(["--idle-timeout-secs", "120"])
        .env("HOME", live.home())
        .env_remove("TRUSTY_SECRETS_SOCKET")
        .env_remove("TRUSTY_SECRETS_INDEX_DIR")
        .env_remove("TRUSTY_SECRETS_IDLE_TIMEOUT_SECS")
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(live.path("server.log")).unwrap())
        .spawn()
        .unwrap();
    live.server = Some(server);
    let started = Instant::now();
    while !runtime().block_on(trusty_common::uds::socket_is_serving(
        &live.socket(),
        Duration::from_millis(200),
    )) {
        if started.elapsed() > Duration::from_secs(20) {
            let log = std::fs::read_to_string(live.path("server.log")).unwrap_or_default();
            panic!("never served; server stderr: {log}");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    live
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

/// `secrets.grant` of [`KEY`] for `child`, as tm would send it; the token.
fn grant(live: &Live, child: u32) -> String {
    let params = json!({"project": live.path("repo"), "child_pid": child,
        "keys": [KEY], "ttl_secs": 120});
    let request = json!({"jsonrpc": "2.0", "id": 1, "method": "secrets.grant", "params": params});
    let response: RpcResponse = runtime()
        .block_on(send_framed_request(
            &live.socket(),
            &request,
            Duration::from_secs(30),
        ))
        .unwrap();
    assert!(
        response.error.is_none(),
        "grant refused: {:?}",
        response.error
    );
    response.result.unwrap()["token"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// Re-run this binary's test in `role`.
fn role(role: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            TEST,
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(ROLE, role);
    command
}

/// A resolve outcome as booleans and fixed error text; never the value.
fn outcome(result: &Result<SecretValue, GrantedError>) -> Value {
    match result {
        Ok(value) => json!({"ok": value.expose() == VALUE}),
        Err(error) => json!({"ok": false, "error": error.to_string()}),
    }
}

fn report(name: &str, body: &Value) {
    let dir = PathBuf::from(std::env::var(OUT).unwrap());
    std::fs::write(dir.join(format!("{name}.json")), body.to_string()).unwrap();
}

fn read_report(dir: &Path, name: &str) -> Value {
    let text = std::fs::read_to_string(dir.join(format!("{name}.json")))
        .unwrap_or_else(|e| panic!("no {name} report: {e}"));
    serde_json::from_str(&text).unwrap()
}

fn parent_pid() -> u32 {
    // SAFETY: `getppid` takes no arguments and cannot fail.
    u32::try_from(unsafe { libc::getppid() }).unwrap()
}

/// Why: AC1 and AC11 against the real binary — a process holding the token
/// fd resolves the stored value with `granted::resolve`, and its own child
/// resolves too, through the token put back on the same fd number.
/// Test: itself.
#[test]
#[ignore = "live: spawns the real binary and a process tree; run on EVO"]
fn granted_client_resolves_through_the_real_binary() {
    let key = SecretKey::new(KEY).unwrap();
    match std::env::var(ROLE).as_deref() {
        Ok("child") => {
            let mut go = String::new();
            std::io::stdin().lock().read_line(&mut go).unwrap();
            let first = granted::resolve(&key);
            let second = granted::resolve(&key);
            let grandchild = role("grandchild").status().unwrap();
            report(
                "child",
                &json!({"pid": std::process::id(), "first": outcome(&first),
                    "second": outcome(&second), "grandchild_exit_ok": grandchild.success()}),
            );
            return;
        }
        Ok("grandchild") => {
            let result = granted::resolve(&key);
            report(
                "grandchild",
                &json!({"pid": std::process::id(), "ppid": parent_pid(),
                    "outcome": outcome(&result)}),
            );
            return;
        }
        _ => {}
    }
    let live = live();
    let out = live.path("out");
    std::fs::create_dir(&out).unwrap();
    let (reader, mut writer) = std::io::pipe().unwrap();
    let fd = reader.as_raw_fd();
    // SAFETY: `F_SETFD` on a pipe end this test owns; the child inherits it.
    assert_eq!(unsafe { libc::fcntl(fd, libc::F_SETFD, 0) }, 0);
    let mut child = role("child")
        .env("HOME", live.home())
        .env(TOKEN_FD_ENV, fd.to_string())
        .env(OUT, &out)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    drop(reader);
    let token = grant(&live, child.id());
    writer.write_all(token.as_bytes()).unwrap();
    drop(writer);
    let mut go = child.stdin.take().unwrap();
    go.write_all(b"go\n").unwrap();
    drop(go);
    assert!(child.wait().unwrap().success(), "child role");

    let got = read_report(&out, "child");
    println!("child: {got}");
    assert_eq!(got["first"], json!({"ok": true}));
    assert_eq!(got["second"], json!({"ok": true}));
    assert_eq!(got["grandchild_exit_ok"], true);
    let grandchild = read_report(&out, "grandchild");
    println!("grandchild: {grandchild}");
    assert_eq!(grandchild["outcome"], json!({"ok": true}));
    assert_eq!(grandchild["ppid"], got["pid"]);

    let resolves: Vec<Value> = live
        .records()
        .into_iter()
        .filter(|r| r["method"] == "secrets.resolve")
        .collect();
    assert_eq!(resolves.len(), 3, "{resolves:?}");
    for (record, pid) in resolves
        .iter()
        .zip([&got["pid"], &got["pid"], &grandchild["pid"]])
    {
        assert_eq!(record["decision"], "allow");
        assert_eq!(record["caller_pid"], *pid);
    }
}
