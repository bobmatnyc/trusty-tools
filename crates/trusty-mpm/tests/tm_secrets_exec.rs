//! `tm secrets exec` against a recording fake trusty-secrets socket (#9621).
//!
//! Why: the spawn, grant, token handoff, exec-in-place, scrub, signal and
//! revoke paths only exist in the BINARY: tm spawns itself as the stub. A
//! fake server sees every `secrets.grant`, `secrets.resolve` and
//! `secrets.revoke` frame with its params, so each test can assert which pid
//! was granted, how many resolves ran and that the grant was revoked.
//! What: [`Fake`] serves the socket under a scratch `$HOME`; [`Fixture::tm`]
//! runs `tm secrets exec` there with `TRUSTY_SECRETS_EXTERNAL=1`, so no real
//! trusty-secrets is ever spawned. Targets are `sh -c` scripts that write
//! what they see to files in the fixture, never a value into an argv.
//! Test: this file IS the test module.

use crate::common;

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::{Value, json};
use trusty_common::uds::server::{RpcError, RpcFallback, RpcRouter, RpcServeOptions, serve_until};

/// The injected value; long enough to be scrubbed.
const VALUE: &str = "sk-live-9621-exec-c0ffee-SENTINEL";
/// The token the fake grants: 64 lowercase hex digits.
const TOKEN: &str = "9621aaaabbbbccccddddeeeeffff00001111222233334444555566667777beef";

/// What the fake answers to `secrets.grant`.
#[derive(Clone)]
enum GrantMode {
    Token(&'static str),
    Refuse,
}

/// The fake's script.
#[derive(Clone)]
struct Script {
    grant: GrantMode,
    /// Sleep this long inside `secrets.grant`, before answering.
    grant_delay: Duration,
    /// SIGKILL the granted pid during that sleep.
    kill_child_in_grant: bool,
    /// Checked for existence before and after the sleep.
    marker: Option<PathBuf>,
    /// key → value for `secrets.resolve`.
    values: BTreeMap<&'static str, &'static str>,
}

impl Script {
    fn new() -> Self {
        Self {
            grant: GrantMode::Token(TOKEN),
            grant_delay: Duration::ZERO,
            kill_child_in_grant: false,
            marker: None,
            values: BTreeMap::from([("GH_TOKEN", VALUE)]),
        }
    }
}

/// Every frame the fake saw.
#[derive(Default, Debug)]
struct Seen {
    grants: Vec<Value>,
    resolves: Vec<String>,
    revokes: Vec<String>,
    /// The marker existed: [before the grant sleep, after it].
    marker_in_grant: Vec<bool>,
}

struct Fake {
    script: Script,
    seen: Arc<Mutex<Seen>>,
}

#[async_trait]
impl RpcFallback for Fake {
    async fn call(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        let refused = || RpcError::new(-32010, "grant refused");
        match method {
            "secrets.grant" => {
                self.seen.lock().unwrap().grants.push(params.clone());
                let exists = || self.script.marker.as_ref().is_some_and(|m| m.exists());
                let before = exists();
                if self.script.kill_child_in_grant {
                    let pid = params["child_pid"].as_i64().unwrap_or(0) as libc::pid_t;
                    // SAFETY: plain syscall; the pid is the stub tm spawned.
                    unsafe { libc::kill(pid, libc::SIGKILL) };
                }
                tokio::time::sleep(self.script.grant_delay).await;
                self.seen
                    .lock()
                    .unwrap()
                    .marker_in_grant
                    .extend([before, exists()]);
                match self.script.grant {
                    GrantMode::Token(token) => {
                        Ok(json!({"token": token, "expires_at": 0, "ttl_secs": 3600}))
                    }
                    GrantMode::Refuse => Err(refused()),
                }
            }
            "secrets.resolve" => {
                let key = params["key"].as_str().unwrap_or_default().to_owned();
                self.seen.lock().unwrap().resolves.push(key.clone());
                if params["token"] != json!(TOKEN) {
                    return Err(refused());
                }
                match self.script.values.get(key.as_str()) {
                    Some(value) => Ok(json!({"key": key, "value": value})),
                    None => Err(RpcError::new(-32004, "not found")),
                }
            }
            "secrets.revoke" => {
                let token = params["token"].as_str().unwrap_or_default().to_owned();
                self.seen.lock().unwrap().revokes.push(token);
                Ok(json!({"revoked": true}))
            }
            _ => Err(RpcError::new(-32601, "method not found")),
        }
    }
}

/// A scratch `$HOME` with the fake serving its trusty-secrets socket.
struct Fixture {
    home: tempfile::TempDir,
    seen: Arc<Mutex<Seen>>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
}

impl Fixture {
    async fn new(script: Script) -> Self {
        // Under /tmp: a macOS temp dir plus the socket subpath nears 104 bytes.
        let home = tempfile::Builder::new()
            .prefix("tsx")
            .tempdir_in("/tmp")
            .expect("home");
        let dir = home.path().join(".trusty-tools/trusty-secrets");
        std::fs::create_dir_all(&dir).expect("socket dir");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).expect("chmod");
        let listener = trusty_common::uds::bind_hardened(&dir.join("secrets.sock")).expect("bind");
        let seen = Arc::new(Mutex::new(Seen::default()));
        let router = Arc::new(RpcRouter::new().fallback(Fake {
            script,
            seen: Arc::clone(&seen),
        }));
        let (stop, rx) = tokio::sync::oneshot::channel::<()>();
        tokio::spawn(async move {
            serve_until(&listener, router, RpcServeOptions::default(), async {
                let _ = rx.await;
            })
            .await;
        });
        Self {
            home,
            seen,
            stop: Some(stop),
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.home.path().join(name)
    }

    /// `tm secrets exec <args>` in the fixture, not yet spawned.
    fn tm(&self, args: &[&str]) -> tokio::process::Command {
        let mut cmd = common::tm_command_in(self.home.path());
        cmd.args(["secrets", "exec"])
            .args(args)
            .current_dir(self.home.path())
            .env("TRUSTY_SECRETS_EXTERNAL", "1")
            .env("OUT", self.path("out"))
            .env("MARK", self.path("mark"))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        tokio::process::Command::from(cmd)
    }

    async fn run(&self, args: &[&str]) -> Output {
        let child = self.tm(args).spawn().expect("spawn tm");
        tokio::time::timeout(Duration::from_secs(60), child.wait_with_output())
            .await
            .expect("tm finished in 60 s")
            .expect("tm output")
    }

    fn seen(&self) -> std::sync::MutexGuard<'_, Seen> {
        self.seen.lock().unwrap()
    }

    fn read(&self, name: &str) -> String {
        std::fs::read_to_string(self.path(name)).unwrap_or_default()
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// The value as `od -An -tx1` prints it, whitespace removed.
fn hex(value: &str) -> String {
    value.bytes().map(|b| format!("{b:02x}")).collect()
}

/// Wait until `path` exists, at most 20 s.
async fn wait_for(path: &Path) {
    let start = Instant::now();
    while !path.exists() {
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "{} never appeared",
            path.display()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn alive(pid: i64) -> bool {
    // SAFETY: signal 0 only checks that the pid exists.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

/// Why: tier 1 end to end — the value reaches the env, output is scrubbed,
/// one resolve per key, and the grant is revoked on a normal exit.
#[tokio::test(flavor = "multi_thread")]
async fn exec_runs_the_command_with_the_value_and_revokes() {
    let fx = Fixture::new(Script::new()).await;
    let script = r#"printf %s "$FOO" | od -An -tx1 > "$OUT"; echo "$FOO"; echo "$FOO" >&2"#;
    let out = fx
        .run(&["--env", "FOO=GH_TOKEN", "--", "sh", "-c", script])
        .await;
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert!(out.status.success(), "{stderr}");
    let seen_hex: String = fx.read("out").split_whitespace().collect();
    assert_eq!(seen_hex, hex(VALUE), "the command did not see the value");
    assert_eq!(stdout, "[REDACTED]\n");
    assert_eq!(stderr, "[REDACTED]\n");
    let seen = fx.seen();
    assert_eq!(seen.grants.len(), 1);
    assert_eq!(seen.grants[0]["keys"], json!(["GH_TOKEN"]));
    assert_eq!(seen.resolves, ["GH_TOKEN"], "one resolve per key");
    assert_eq!(seen.revokes, [TOKEN], "revoked once, on exit");
}

/// Why: the grant must name the stub, which then becomes the command.
#[tokio::test(flavor = "multi_thread")]
async fn exec_grant_names_the_stub_pid_not_tm() {
    let fx = Fixture::new(Script::new()).await;
    let child = fx
        .tm(&[
            "--env",
            "FOO=GH_TOKEN",
            "--",
            "sh",
            "-c",
            r#"echo "$$ $PPID" > "$OUT""#,
        ])
        .spawn()
        .expect("spawn tm");
    let tm_pid = i64::from(child.id().expect("tm pid"));
    let out = child.wait_with_output().await.expect("tm output");
    assert!(out.status.success(), "{}", text(&out.stderr));
    let ids: Vec<i64> = fx
        .read("out")
        .split_whitespace()
        .map(|s| s.parse().unwrap())
        .collect();
    let granted = fx.seen().grants[0]["child_pid"]
        .as_i64()
        .expect("child_pid");
    assert_ne!(granted, tm_pid, "the grant named tm itself");
    assert_eq!(
        granted, ids[0],
        "the grant must name the process that became the command"
    );
    assert_eq!(
        ids[1], tm_pid,
        "the command is tm's child: the stub exec'd in place"
    );
}

/// Why: the stub must not run the command before the token reaches it.
#[tokio::test(flavor = "multi_thread")]
async fn exec_never_execs_before_the_token() {
    let mut script = Script::new();
    script.grant_delay = Duration::from_millis(1500);
    let fx_marker = tempfile::tempdir().expect("dir");
    let mark = fx_marker.path().join("mark");
    script.marker = Some(mark.clone());
    let fx = Fixture::new(script).await;
    let target = format!("touch '{}'", mark.display());
    let out = fx
        .run(&["--env", "FOO=GH_TOKEN", "--", "sh", "-c", &target])
        .await;
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        fx.seen().marker_in_grant,
        [false, false],
        "ran before the grant answered"
    );
    assert!(mark.exists(), "the command never ran");
}

/// Why: a refused grant runs nothing and leaves no stub behind.
#[tokio::test(flavor = "multi_thread")]
async fn exec_kills_the_stub_group_when_the_grant_is_refused() {
    let mut script = Script::new();
    script.grant = GrantMode::Refuse;
    let fx = Fixture::new(script).await;
    let out = fx
        .run(&[
            "--env",
            "FOO=GH_TOKEN",
            "--",
            "sh",
            "-c",
            r#"touch "$MARK""#,
        ])
        .await;
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("grant refused") && stderr.contains("nothing ran"),
        "{stderr}"
    );
    assert!(!fx.path("mark").exists(), "the command ran");
    let seen = fx.seen();
    assert!(seen.resolves.is_empty());
    let stub = seen.grants[0]["child_pid"].as_i64().unwrap();
    assert!(!alive(stub), "the stub outlived tm");
}

/// Why: one key that does not resolve runs nothing, names the key and
/// still revokes the grant.
#[tokio::test(flavor = "multi_thread")]
async fn exec_fails_closed_when_a_key_does_not_resolve() {
    let fx = Fixture::new(Script::new()).await;
    let args = [
        "--env",
        "A=GH_TOKEN",
        "--env",
        "B=MISSING",
        "--",
        "sh",
        "-c",
        r#"touch "$MARK""#,
    ];
    let out = fx.run(&args).await;
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("MISSING did not resolve") && stderr.contains("nothing ran"),
        "{stderr}"
    );
    assert!(!stderr.contains(VALUE) && !text(&out.stdout).contains(VALUE));
    assert!(
        !fx.path("mark").exists(),
        "the command ran with a partial env"
    );
    assert_eq!(fx.seen().revokes, [TOKEN]);
}

/// Why: a short token is never handed on; nothing resolves, nothing runs.
#[tokio::test(flavor = "multi_thread")]
async fn exec_fails_closed_on_a_malformed_token() {
    let mut script = Script::new();
    script.grant = GrantMode::Token("abc123");
    let fx = Fixture::new(script).await;
    let out = fx
        .run(&[
            "--env",
            "FOO=GH_TOKEN",
            "--",
            "sh",
            "-c",
            r#"touch "$MARK""#,
        ])
        .await;
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("malformed"), "{stderr}");
    assert!(!fx.path("mark").exists());
    let seen = fx.seen();
    assert!(seen.resolves.is_empty());
    assert_eq!(
        seen.revokes,
        ["abc123"],
        "a malformed grant is still revoked"
    );
}

/// Why: a stub that dies before exec leaves the grant revoked and runs nothing.
#[tokio::test(flavor = "multi_thread")]
async fn exec_revokes_when_the_stub_dies_before_exec() {
    let mut script = Script::new();
    script.kill_child_in_grant = true;
    script.grant_delay = Duration::from_millis(300);
    let fx = Fixture::new(script).await;
    let out = fx
        .run(&[
            "--env",
            "FOO=GH_TOKEN",
            "--",
            "sh",
            "-c",
            r#"touch "$MARK""#,
        ])
        .await;
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("nothing ran"), "{stderr}");
    assert!(!fx.path("mark").exists());
    assert_eq!(fx.seen().revokes, [TOKEN]);
}

/// Why: SIGTERM to tm reaches the command, and the grant is revoked.
#[tokio::test(flavor = "multi_thread")]
async fn exec_forwards_sigterm_and_revokes() {
    let fx = Fixture::new(Script::new()).await;
    let child = fx
        .tm(&[
            "--env",
            "FOO=GH_TOKEN",
            "--",
            "sh",
            "-c",
            r#"touch "$MARK"; exec sleep 30"#,
        ])
        .spawn()
        .expect("spawn tm");
    let tm_pid = child.id().expect("tm pid") as libc::pid_t;
    wait_for(&fx.path("mark")).await;
    // SAFETY: plain syscall to the tm this test spawned.
    unsafe { libc::kill(tm_pid, libc::SIGTERM) };
    let out = tokio::time::timeout(Duration::from_secs(15), child.wait_with_output())
        .await
        .expect("tm exited after SIGTERM")
        .expect("tm output");
    assert_eq!(
        out.status.code(),
        Some(128 + libc::SIGTERM),
        "{}",
        text(&out.stderr)
    );
    let seen = fx.seen();
    assert_eq!(seen.revokes, [TOKEN]);
    assert!(!alive(seen.grants[0]["child_pid"].as_i64().unwrap()));
}

/// Why: a command killed by a signal reports 128 + signal and is revoked.
#[tokio::test(flavor = "multi_thread")]
async fn exec_reports_a_sigkilled_command_and_revokes() {
    let fx = Fixture::new(Script::new()).await;
    let out = fx
        .run(&["--env", "FOO=GH_TOKEN", "--", "sh", "-c", "kill -9 $$"])
        .await;
    assert_eq!(out.status.code(), Some(137), "{}", text(&out.stderr));
    assert_eq!(fx.seen().revokes, [TOKEN]);
}

/// Why: the command keeps its exit status when the server is gone at
/// revoke time; tm says the grant lives to its TTL.
#[tokio::test(flavor = "multi_thread")]
async fn exec_survives_a_server_that_dies_mid_run() {
    let mut fx = Fixture::new(Script::new()).await;
    let child = fx
        .tm(&[
            "--env",
            "FOO=GH_TOKEN",
            "--",
            "sh",
            "-c",
            r#"touch "$MARK"; sleep 1"#,
        ])
        .spawn()
        .expect("spawn tm");
    wait_for(&fx.path("mark")).await;
    drop(fx.stop.take());
    let _ = std::fs::remove_file(fx.path(".trusty-tools/trusty-secrets/secrets.sock"));
    let out = child.wait_with_output().await.expect("tm output");
    let stderr = text(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert!(stderr.contains("could not be revoked"), "{stderr}");
}

/// Why: no argv on the machine carries the value or the token, the command's
/// env carries only the token's fd number, and that fd yields the token.
#[tokio::test(flavor = "multi_thread")]
async fn exec_argv_never_carries_a_value_or_token() {
    let fx = Fixture::new(Script::new()).await;
    let script = r#"ps -A -ww -o args= > "$OUT"; env > "$OUT.env"; cat <&"$TRUSTY_SECRETS_TOKEN_FD" > "$OUT.tok""#;
    let out = fx
        .run(&["--env", "FOO=GH_TOKEN", "--", "sh", "-c", script])
        .await;
    assert!(out.status.success(), "{}", text(&out.stderr));
    let ps = fx.read("out");
    assert!(
        ps.contains("__exec-stub") || ps.contains("secrets exec"),
        "ps saw no tm: {ps}"
    );
    assert!(!ps.contains(VALUE), "an argv carries the value");
    assert!(!ps.contains(TOKEN), "an argv carries the token");
    let env = fx.read("out.env");
    assert!(!env.contains(TOKEN), "the command's env carries the token");
    assert!(env.contains("TRUSTY_SECRETS_TOKEN_FD="), "{env}");
    assert_eq!(fx.read("out.tok"), TOKEN, "tier 3: the fd yields the token");
}

/// Why: on a terminal nothing is scrubbed (Architect ruling Q3).
#[tokio::test(flavor = "multi_thread")]
async fn exec_leaves_terminal_output_unscrubbed() {
    let fx = Fixture::new(Script::new()).await;
    let (mut master, mut slave) = (0, 0);
    // SAFETY: openpty writes two fds into the out-params.
    let rc = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    assert_eq!(rc, 0, "openpty");
    // SAFETY: both fds were just opened and are owned here.
    let (master, slave) = unsafe {
        use std::os::fd::FromRawFd;
        (
            std::fs::File::from_raw_fd(master),
            std::fs::File::from_raw_fd(slave),
        )
    };
    let mut cmd = fx.tm(&["--env", "FOO=GH_TOKEN", "--", "sh", "-c", r#"echo "$FOO""#]);
    cmd.stdout(slave);
    let out = cmd.output().await.expect("tm output");
    assert!(out.status.success(), "{}", text(&out.stderr));
    let shown = tokio::task::spawn_blocking(move || {
        use std::io::Read;
        let mut master = master;
        let mut all = Vec::new();
        let mut buf = [0u8; 4096];
        // Linux answers EIO once the slave side is closed.
        while let Ok(n) = master.read(&mut buf) {
            if n == 0 {
                break;
            }
            all.extend_from_slice(&buf[..n]);
            if String::from_utf8_lossy(&all).contains('\n') {
                break;
            }
        }
        all
    })
    .await
    .expect("read pty");
    assert!(
        text(&shown).contains(VALUE),
        "terminal output was scrubbed: {}",
        text(&shown)
    );
}
