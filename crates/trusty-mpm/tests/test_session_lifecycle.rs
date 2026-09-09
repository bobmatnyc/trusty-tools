//! Full user-cycle end-to-end test for the session lifecycle.
//!
//! Why: the per-handler unit tests drive functions directly and the `e2e`
//! suite covers individual scenario areas. This test walks the *entire*
//! operator-facing path — start, command, pause, resume, stop — over the live
//! HTTP API in one continuous flow, the way the CLI / TUI / Telegram bot drive
//! the daemon.
//! What: a standalone integration-test binary that binds the daemon's axum
//! router to a random loopback port and exercises the lifecycle with `reqwest`.
//! Test: `cargo test -p trusty-mpm-daemon --test test_session_lifecycle`.

use std::net::SocketAddr;
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use trusty_mpm::core::host_state_gate::ALLOW_HOST_STATE_ENV;
use trusty_mpm::daemon::state::DaemonState;
use trusty_mpm::daemon::tmux::TmuxDriver;

/// RAII override of one environment variable, restored on drop.
///
/// Why: `$HOME` is process-global, and an assertion failure between the set and
/// a manual restore would leak the scratch value into the harness's own
/// teardown. Same shape as `scratch_home_tmux_gate.rs`'s guard — this binary
/// holds exactly ONE test (`full_user_cycle`), which is what makes a plain
/// process-global override safe here without `#[serial_test::file_serial]`:
/// under both `cargo test` and `cargo nextest` no other test shares the process.
struct EnvOverride {
    key: &'static str,
    prev: Option<std::ffi::OsString>,
}

impl EnvOverride {
    fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
        let prev = std::env::var_os(key);
        // SAFETY: single-test binary — no other thread reads or writes the
        // environment while this runs.
        unsafe { std::env::set_var(key, value) };
        Self { key, prev }
    }

    /// Remove `key` for the duration, restoring it on drop.
    ///
    /// Why (#7244): `TMUX` is inherited whenever the harness itself runs inside
    /// tmux, and tmux attaches to the server that variable names in preference
    /// to `TMUX_TMPDIR`. Leaving it set kept every `tmux` call this test makes
    /// pointed at the OPERATOR's server, whose live `tm-*` sessions the delete
    /// step then adopted and re-prepared — writing their real workspaces'
    /// `.claude/settings.json`. Setting it empty is not equivalent: tmux treats
    /// an empty `TMUX` as a malformed server address, not as absent.
    fn remove(key: &'static str) -> Self {
        let prev = std::env::var_os(key);
        // SAFETY: as in `set` — single-test binary.
        unsafe { std::env::remove_var(key) };
        Self { key, prev }
    }
}

impl Drop for EnvOverride {
    fn drop(&mut self) {
        // SAFETY: as in `set` — single-test binary.
        match self.prev.take() {
            Some(v) => unsafe { std::env::set_var(self.key, v) },
            None => unsafe { std::env::remove_var(self.key) },
        }
    }
}

/// RAII override of the process working directory, restored on drop.
///
/// Why (#7244): `$HOME` was already redirected (#6523), but the project-tier
/// settings writer resolves its target from the CHECKOUT, not from `$HOME`, and
/// `cargo test` runs this binary with its cwd inside the repository. A run from
/// an agent worktree therefore rewrote the real project's
/// `.claude/settings.json` — every hook, including pm-guard and the Read/Bash
/// diversion, repointed at this test binary. Redirecting cwd puts any such
/// resolution inside the scratch directory instead.
/// What: same shape as [`EnvOverride`], for `std::env::current_dir`. Restores
/// the original directory on drop so the harness's own teardown is unaffected.
/// Test: `full_user_cycle`, whose [`RealProjectSettingsGuard`] fails the test if
/// this redirect ever stops working.
struct CwdOverride {
    prev: std::path::PathBuf,
}

impl CwdOverride {
    fn set(dir: &Path) -> Self {
        let prev = std::env::current_dir().expect("resolve cwd");
        std::env::set_current_dir(dir).expect("redirect cwd into the scratch dir");
        Self { prev }
    }
}

impl Drop for CwdOverride {
    fn drop(&mut self) {
        let _ = std::env::set_current_dir(&self.prev);
    }
}

/// A tripwire on the REAL checkout's `.claude/settings.json` files.
///
/// Why (#7244): hermeticity that is only asserted by construction rots — the
/// #6523 `$HOME` redirect was in place and the write still happened, through a
/// path nobody had thought to redirect. This watches the actual files: if any
/// future change reaches a real settings file again, the test that reached it
/// fails, naming the file, rather than the damage being noticed days later in a
/// dead pm-guard.
/// What: snapshots the content of `<root>/.claude/settings.json` for BOTH roots
/// a `cargo test` run can resolve — this checkout, and (when this checkout is a
/// linked worktree) the main checkout its `.git` file points at, which is the
/// one #7244 actually clobbered. `check` compares content, so a rewrite with
/// identical bytes is correctly not a failure, and an absent file that appears
/// is. Runs on drop too, so a panicking test still reports a clobber it caused.
/// Test: `full_user_cycle`.
struct RealProjectSettingsGuard {
    before: Vec<(std::path::PathBuf, Option<String>)>,
}

impl RealProjectSettingsGuard {
    /// Snapshot every real settings file this run could reach.
    fn arm() -> Self {
        let before = Self::watched_paths()
            .into_iter()
            .map(|p| {
                let content = std::fs::read_to_string(&p).ok();
                (p, content)
            })
            .collect();
        Self { before }
    }

    /// The `.claude/settings.json` of this checkout and of its main checkout.
    ///
    /// `CARGO_MANIFEST_DIR` is `<checkout>/crates/trusty-mpm`, so two `pop`s
    /// reach the checkout root without depending on the cwd this test redirects.
    /// A linked worktree's `.git` is a FILE reading
    /// `gitdir: <main>/.git/worktrees/<name>`; trimming the last two components
    /// yields `<main>/.git`, whose parent is the main checkout — resolved by
    /// reading the file rather than spawning `git`, so the guard works with no
    /// git binary and cannot be confused by the redirected cwd.
    fn watched_paths() -> Vec<std::path::PathBuf> {
        let mut roots = Vec::new();
        let mut checkout = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        checkout.pop();
        checkout.pop();

        if let Ok(text) = std::fs::read_to_string(checkout.join(".git"))
            && let Some(rest) = text.trim().strip_prefix("gitdir:")
        {
            let mut admin = std::path::PathBuf::from(rest.trim());
            // `<main>/.git/worktrees/<name>` -> `<main>`
            admin.pop();
            admin.pop();
            if admin.file_name() == Some(std::ffi::OsStr::new(".git"))
                && let Some(main_root) = admin.parent()
            {
                roots.push(main_root.to_path_buf());
            }
        }
        roots.push(checkout);
        roots
            .into_iter()
            .map(|r| r.join(".claude").join("settings.json"))
            .collect()
    }

    /// Panic naming any real settings file whose content changed.
    fn check(&self) {
        let changed: Vec<String> = self
            .before
            .iter()
            .filter(|(path, before)| std::fs::read_to_string(path).ok().as_ref() != before.as_ref())
            .map(|(path, _)| path.display().to_string())
            .collect();
        assert!(
            changed.is_empty(),
            "this test wrote the REAL project's Claude settings (#7244) — every hook \
             in these files now points wherever this run resolved, and pm-guard \
             enforcement is dead until they are regenerated: {changed:#?}"
        );
    }
}

impl Drop for RealProjectSettingsGuard {
    fn drop(&mut self) {
        // A double panic aborts the process, so an already-failing test reports
        // its own failure rather than this one.
        if !std::thread::panicking() {
            self.check();
        }
    }
}

/// True when a tmux session named `name` is live on this host.
///
/// Why: the #1454 assertion ("DELETE killed the tmux host") is only meaningful
/// where tmux exists; this lets the test create a real host, then prove it is
/// gone after DELETE, while still passing on tmux-less CI.
/// What: lists live tmux sessions via the daemon driver and reports whether
/// `name` is present; any discovery/listing error is treated as "not present".
/// Test: exercised by `full_user_cycle` when tmux is available.
fn tmux_session_live(name: &str) -> bool {
    match TmuxDriver::discover() {
        Ok(driver) => driver
            .list_sessions()
            .map(|sessions| sessions.iter().any(|s| s.name == name))
            .unwrap_or(false),
        Err(_) => false,
    }
}

/// A live daemon for this test, bound to a random loopback port.
///
/// Why: the lifecycle test needs a real HTTP endpoint; bundling the server task
/// handle lets `Drop` abort it so the port is released between tests.
/// What: holds the base URL and the background server task handle.
struct TestServer {
    url: String,
    handle: tokio::task::JoinHandle<()>,
}

impl TestServer {
    /// Bind the daemon router to `127.0.0.1:0` and wait until it is healthy.
    ///
    /// Why: `axum::serve` accepts connections asynchronously; firing a request
    /// the instant after spawn can race the listener, so we poll `/health`.
    /// What: builds an in-memory `DaemonState`, serves the router on a task, and
    /// blocks until `GET /health` returns `200` (max ~2s).
    /// #7244: `framework_root` is INJECTED rather than resolved. `DaemonState`
    /// derives the managed session-manager store from its framework root, and
    /// `DELETE /sessions/{id}` reconciles that store on the way out —
    /// re-preparing every session it lists, which runs the project-tier
    /// settings writer against each one's workspace. Resolving the root led to
    /// the OPERATOR's real store, so the delete step re-prepared the real
    /// checkout and rewrote its `.claude/settings.json`. A redirected `$HOME`
    /// did not stop it; only naming the root does.
    async fn spawn(framework_root: &Path) -> Self {
        let state = std::sync::Arc::new(DaemonState::with_root(framework_root.to_path_buf()));
        let app = trusty_mpm::daemon::api::router(state);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback port");
        let addr: SocketAddr = listener.local_addr().expect("resolve bound addr");
        let handle = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let url = format!("http://{addr}");
        wait_for_health(&url).await;
        Self { url, handle }
    }

    /// Build an absolute URL for `path` (which should start with `/`).
    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.url)
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

/// Poll `GET /health` until the daemon answers, or panic after ~2s.
async fn wait_for_health(base: &str) {
    let client = reqwest::Client::new();
    let deadline = Instant::now() + Duration::from_secs(2);
    let health = format!("{base}/health");
    loop {
        if let Ok(resp) = client.get(&health).send().await
            && resp.status().is_success()
        {
            return;
        }
        if Instant::now() >= deadline {
            panic!("daemon at {base} did not become healthy within 2s");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Full user cycle: start → command (API-only) → pause → resume → stop.
///
/// Why: end-to-end validation that the session lifecycle flows correctly
/// through the HTTP API — the operator-facing path for the full user cycle.
/// What: starts a daemon with an in-memory test state, walks the full
/// lifecycle via HTTP, asserts each transition.
/// Note: tmux send/capture are not exercised (tmux may not be installed in CI);
/// the command and output endpoints are called but the underlying driver ops
/// are best-effort and errors are logged rather than propagated to the test.
/// `$HOME` is redirected to a scratch directory for the whole run (#6523), so
/// the pause step writes its `pause.json` there rather than into the operator's
/// real `~/.trusty-mpm/sessions/`.
#[tokio::test]
async fn full_user_cycle() {
    // #6523: `$HOME` is both the daemon's data root (`~/.trusty-mpm`) and the
    // base `core::session_store::pause_path` resolves, so step 5's pause wrote a
    // real `~/.trusty-mpm/sessions/<uuid>/pause.json` into the operator's home
    // on every run and nothing removed it. Set before `TestServer::spawn`, which
    // resolves `FrameworkPaths::default()` at construction.
    // #7244: armed FIRST, so it covers everything below including the daemon
    // construction, and fires on drop even if an assertion panics.
    let _real_settings = RealProjectSettingsGuard::arm();

    let scratch_home = tempfile::tempdir().expect("scratch home");
    let _home = EnvOverride::set("HOME", scratch_home.path());
    // #7244: `$HOME` is not the only root a settings write resolves — the
    // project-tier writer starts from the CHECKOUT, and `cargo test` runs this
    // binary inside it. Redirecting cwd sends any such resolution into the
    // scratch directory. Safe as a process-global here for the same reason
    // `EnvOverride` is: this binary holds exactly one test.
    let _cwd = CwdOverride::set(scratch_home.path());
    // #6523: a scratch `$HOME` is precisely what `host_state_gate` refuses tmux
    // under (#5784), and step 11's #1454 assertion needs a REAL tmux host to
    // mean anything — without the opt-in `tmux_available` reads false and the
    // whole tmux arm silently vanishes. This is the hatch that gate documents
    // for a test that deliberately wants an isolated `$HOME` AND real tmux.
    let _allow_host_state = EnvOverride::set(ALLOW_HOST_STATE_ENV, "1");
    // #7244: that opt-in is what made this test non-hermetic. Step 9's DELETE
    // reconciles the managed store, which ADOPTS live `tm-*` tmux sessions it
    // does not know and re-prepares each one's workspace — running the
    // project-tier settings writer over the operator's real checkouts. With
    // real tmux allowed, the operator's own sessions were in scope, so a
    // `cargo test` run rewrote the real project's `.claude/settings.json`.
    // `TMUX_TMPDIR` gives this test its own tmux server, so the only session
    // reconciliation can see is the one this test creates. Step 11's #1454
    // assertion still runs against real tmux, which is what #6523 wanted.
    let _tmux_tmpdir = EnvOverride::set("TMUX_TMPDIR", scratch_home.path());
    let _tmux = EnvOverride::remove("TMUX");

    let server = TestServer::spawn(&scratch_home.path().join(".trusty-mpm")).await;
    let client = reqwest::Client::new();

    // 1. Start a session.
    let created = client
        .post(server.url("/sessions"))
        .json(&json!({ "project": "/tmp/lifecycle" }))
        .send()
        .await
        .expect("create session");
    // The handler returns axum::Json, which serialises with a 200 status even
    // though the OpenAPI annotation documents a semantic 201.
    assert!(
        created.status() == 200 || created.status() == 201,
        "create status: {}",
        created.status()
    );
    let created: Value = created.json().await.expect("create body");
    let name = created["name"].as_str().expect("name present").to_string();
    // The friendly name resolves pause/command/output; `DELETE /sessions/{id}`
    // resolves strictly by UUID, so keep the id for the stop step.
    let id = created["id"].as_str().expect("id present").to_string();
    assert!(!id.is_empty());

    // Create a REAL tmux host for this session (when tmux is available) so the
    // #1454 assertion below can prove DELETE actually killed it. Without this
    // the API never spins up a tmux session (no `workdir` was supplied), so the
    // kill would be a vacuous no-op and the leak this test guards would slip by.
    let tmux_available = TmuxDriver::is_available();
    if tmux_available && let Ok(driver) = TmuxDriver::discover() {
        driver
            .create_session(&name, Some("/tmp"))
            .expect("create real tmux host for the session");
        assert!(
            tmux_session_live(&name),
            "precondition: tmux host {name} must be live after create"
        );
    }

    // 2. List sessions — exactly one, in a live state.
    let listed: Value = client
        .get(server.url("/sessions"))
        .send()
        .await
        .expect("list sessions")
        .json()
        .await
        .expect("list body");
    let sessions = listed["sessions"].as_array().expect("sessions array");
    assert_eq!(sessions.len(), 1);
    let status = sessions[0]["status"].as_str().expect("status string");
    assert!(
        status == "Starting" || status == "Active",
        "post-start status: {status}"
    );

    // 3. Send a command, requesting a summarized capture. tmux errors are
    //    swallowed by the handler; the `?compress=summarise` query exercises the
    //    "summarize output" step of the full user cycle.
    let cmd = client
        .post(server.url(&format!("/sessions/{name}/command?compress=summarise")))
        .json(&json!({ "command": "help" }))
        .send()
        .await
        .expect("send command");
    assert_eq!(cmd.status(), 200);
    let cmd_body: Value = cmd.json().await.expect("command body");
    assert_eq!(cmd_body["sent"], true);
    assert!(cmd_body["output"].is_string());
    // A summarized command response carries the compression byte counts.
    assert!(
        cmd_body.get("original_bytes").is_some(),
        "original_bytes key present"
    );
    assert!(
        cmd_body.get("compressed_bytes").is_some(),
        "compressed_bytes key present"
    );

    // 4. Capture output.
    let out = client
        .get(server.url(&format!("/sessions/{name}/output")))
        .send()
        .await
        .expect("get output");
    assert_eq!(out.status(), 200);
    let out_body: Value = out.json().await.expect("output body");
    assert!(out_body.get("output").is_some(), "output key present");

    // 5. Pause the session.
    let paused = client
        .post(server.url(&format!("/sessions/{name}/pause")))
        .json(&json!({ "summary": "mid-task" }))
        .send()
        .await
        .expect("pause session");
    assert_eq!(paused.status(), 200);
    let paused_body: Value = paused.json().await.expect("pause body");
    assert_eq!(paused_body["paused"], true);

    // 6. List — status is now Paused.
    let listed: Value = client
        .get(server.url("/sessions"))
        .send()
        .await
        .expect("list after pause")
        .json()
        .await
        .expect("list body");
    assert_eq!(
        listed["sessions"][0]["status"], "Paused",
        "session must be Paused after pause"
    );

    // 7. Resume the session.
    let resumed = client
        .post(server.url(&format!("/sessions/{name}/resume")))
        .send()
        .await
        .expect("resume session");
    assert_eq!(resumed.status(), 200);

    // 8. List — status is back to a live state, not Paused.
    let listed: Value = client
        .get(server.url("/sessions"))
        .send()
        .await
        .expect("list after resume")
        .json()
        .await
        .expect("list body");
    let status = listed["sessions"][0]["status"]
        .as_str()
        .expect("status string");
    assert!(
        status == "Active" || status == "Starting",
        "post-resume status must be live, got {status}"
    );

    // 9. Stop (delete) the session.
    let deleted = client
        .delete(server.url(&format!("/sessions/{id}")))
        .send()
        .await
        .expect("delete session");
    assert!(
        deleted.status() == 200 || deleted.status() == 204,
        "delete status: {}",
        deleted.status()
    );

    // 10. List — the registry is empty again.
    let listed: Value = client
        .get(server.url("/sessions"))
        .send()
        .await
        .expect("list after stop")
        .json()
        .await
        .expect("list body");
    assert!(
        listed["sessions"]
            .as_array()
            .expect("sessions array")
            .is_empty(),
        "session must be gone after stop"
    );

    // 11. #1454: DELETE must have KILLED the tmux host, not just dropped the
    //     registry entry. When tmux is available the host we created in step 1
    //     must now be gone. Best-effort cleanup if (regression) it lingers, so a
    //     failing run does not leak a session into the next test.
    if tmux_available {
        let still_live = tmux_session_live(&name);
        if still_live && let Ok(driver) = TmuxDriver::discover() {
            let _ = driver.kill_session(&name);
        }
        assert!(
            !still_live,
            "tmux host {name} must be killed by DELETE /sessions/{{id}} (#1454)"
        );
    }

    // 12. #7244: nothing above may have touched the real checkout's Claude
    //     settings. Asserted explicitly here as well as on drop so the failure
    //     names this step rather than an unwind.
    _real_settings.check();
}
