//! Sidecar process management for the `trusty-agents --api` subprocess.
//!
//! Why: The Tauri shell spawns and supervises a `trusty-agents --api`
//! subprocess so the REST server is reachable as soon as the window opens,
//! without asking the user to start a server manually. This module owns the
//! shared state, spawn/respawn logic, binary resolution, and health probing —
//! split out of `main.rs` (#3220 header-consolidation wave; `main.rs` had
//! grown past the workspace's 500-SLOC production cap) so the process-
//! lifecycle concern is self-contained and reusable by both `main.rs` (tray
//! quit / window-close handlers) and `task_commands.rs` (per-request health
//! checks and API base URL).
//! What: `ApiServerState`/`SharedApi`, `api_base`, `ensure_api_server`
//! (Tauri command), `resolve_tagent_binary`, `sidecar_candidate_names`,
//! `http_health`, `kill_sidecar`, `terminate_child`.
//! Test: `cargo check` in `ui/src-tauri/`; the process-spawn/respawn logic
//! itself isn't unit-testable without an actual `tagent` binary on disk (see
//! the module-level doc in the original `main.rs` for prior manual test
//! notes), but the pure candidate-name ordering is covered by the `tests`
//! module at the bottom of this file.
//!
//! Longer term, the sidecar should probably be bundled via Tauri's
//! `externalBin` mechanism (see `tauri.conf.json`) instead of resolved by
//! name search at runtime — that would make packaging self-contained and
//! remove this whole resolution dance. Out of scope here; tracked as a
//! follow-up, not fixed in this pass.

use std::process::ExitStatus;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tauri::{AppHandle, State};
use tokio::process::Child;
use tokio::sync::Mutex;

use crate::sse_bridge::start_event_bridge;

/// How long we let the sidecar shut down gracefully after SIGTERM before we
/// escalate to SIGKILL.
///
/// Why: On macOS the Cmd+Q quit path is a SYNCHRONOUS AppKit teardown
/// (`applicationShouldTerminate:` → reply → exit) that completes in a few
/// milliseconds (#3734 root cause). The Tauri-side reap therefore has to run
/// synchronously inside the quit event and must stay short so it does not make
/// quit feel laggy. 500ms is ample for a healthy `tagent --api` to catch
/// SIGTERM and unbind its listener; anything slower is covered by the sidecar's
/// own parent-death watchdog (#3734), which is the real guarantee against
/// orphans — so a short bounded window here is safe.
pub(crate) const SIDECAR_GRACE: Duration = Duration::from_millis(500);

/// Shared handle to the spawned `trusty-agents --api` sidecar.
///
/// Why: We must not spawn the API sidecar twice; a `Mutex<Option<Child>>`
/// lets us check-and-insert atomically and lets the window-destroy hook
/// kill the child cleanly.
/// What: `None` until `ensure_api_server` spawns the subprocess. `Some(child)`
/// thereafter.
/// Test: Call `ensure_api_server(7654)` twice, assert only one child is
/// spawned (second call short-circuits on `is_some()`).
#[derive(Default)]
pub struct ApiServerState {
    pub child: Mutex<Option<Child>>,
    pub port: Mutex<Option<u16>>,
    /// Set once the desktop SSE→Tauri event bridge has been spawned, so
    /// repeated `ensure_api_server` calls (every `App.svelte::onMount`) start
    /// exactly one long-lived listener.
    pub bridge_started: AtomicBool,
}

pub type SharedApi = Arc<ApiServerState>;

pub fn api_base(port: u16) -> String {
    format!("http://127.0.0.1:{port}")
}

/// Spawn `trusty-agents --api --port <port>` if not already running.
///
/// Why: Lets the frontend operate as soon as the window is visible without
/// asking the user to start a server first. Silently succeeds when the
/// sidecar is already running so repeated calls from `App.svelte::onMount`
/// are safe.
/// What: Checks `/api/health` first — if it responds OK, does nothing. Else
/// spawns `trusty-agents --api --port <port>` (resolved relative to $PATH first,
/// then a workspace-relative debug/release target) and records the `Child`.
/// Test: Call this twice — second call returns early; kill the child and
/// call again — it re-spawns.
#[tauri::command]
pub async fn ensure_api_server(
    app: AppHandle,
    port: u16,
    state: State<'_, SharedApi>,
) -> Result<(), String> {
    // Fast path: if the server already answers, there is nothing to do (but
    // still ensure the streaming bridge is running).
    if http_health(port).await {
        let mut p = state.port.lock().await;
        *p = Some(port);
        drop(p);
        maybe_start_bridge(&app, &state, port);
        return Ok(());
    }

    let mut guard = state.child.lock().await;
    if let Some(ref mut existing) = *guard {
        // Check whether the previously-spawned child is still alive.
        // `try_wait` returns Ok(None) if running, Ok(Some(status)) if exited.
        match existing.try_wait() {
            Ok(Some(_status)) => {
                // Child exited — clear the slot so we can respawn below.
                tracing::warn!(port, "trusty-agents sidecar exited; respawning");
                *guard = None;
            }
            Ok(None) => {
                // Still running but not yet healthy; let caller poll health.
                return Ok(());
            }
            Err(e) => {
                tracing::warn!(?e, "try_wait on sidecar failed; clearing and respawning");
                *guard = None;
            }
        }
    }

    let binary = resolve_tagent_binary();

    // #api-sidecar-cwd: When launched from the macOS .app bundle, the Tauri
    // process's cwd is `/` (sealed read-only APFS volume). The sidecar's
    // self-project detection falls back to cwd when no marker is found, which
    // would result in attempts to create `/.trusty-agents/state` (EROFS). Pass the
    // compile-time-known trusty-agents project root via TAGENT_PROJECT_DIR so the
    // sidecar resolves state dirs and `.env.local` against the correct path.
    //
    // Why: Fix for "API server did not become healthy within 20s" — the
    // sidecar was crashing on `create_dir_all("/.trusty-agents/state")` before
    // binding the HTTP listener.
    // What: Set TAGENT_PROJECT_DIR to the trusty-agents repo root derived from
    // CARGO_MANIFEST_DIR (ui/src-tauri → ../.. → repo root).
    // Test: Launch the bundled .app, observe sidecar reaches /api/health
    // within the 20s polling window.
    let project_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from("."));

    tracing::info!(
        ?binary,
        port,
        ?project_root,
        "spawning trusty-agents --api sidecar"
    );

    // #3734: pass our own pid as `--parent-pid` so the sidecar arms its
    // parent-death watchdog and self-exits if this GUI dies by any path the
    // Tauri quit-event reap can miss (macOS Cmd+Q sudden teardown, a crash, an
    // external SIGTERM/SIGKILL) — no orphan can then hold the port across a
    // GUI restart.
    let child = tokio::process::Command::new(&binary)
        .arg("--api")
        .arg("--port")
        .arg(port.to_string())
        .arg("--parent-pid")
        .arg(std::process::id().to_string())
        .env("TAGENT_PROJECT_DIR", &project_root)
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .map_err(|e| format!("failed to spawn {}: {e}", binary.display()))?;

    *guard = Some(child);
    drop(guard);
    let mut p = state.port.lock().await;
    *p = Some(port);
    drop(p);
    maybe_start_bridge(&app, &state, port);
    Ok(())
}

/// Spawn the desktop SSE→Tauri streaming bridge exactly once.
///
/// Why: `ensure_api_server` is called on every `App.svelte::onMount`, but the
/// bridge is a single long-lived listener that serves every task — spawning it
/// per call would leak duplicate connections all emitting the same
/// `task-delta` events. The `bridge_started` flag makes the first caller win.
/// What: Atomically flips `bridge_started`; on the transition from `false` the
/// caller spawns [`start_event_bridge`]. The connection tolerates the sidecar
/// not being healthy yet — it reconnects with backoff until `/api/events`
/// answers.
/// Test: exercised manually (desktop launch → streamed bubble); the parser it
/// drives is unit-tested in `sse_bridge::tests`.
fn maybe_start_bridge(app: &AppHandle, state: &SharedApi, port: u16) {
    if !state.bridge_started.swap(true, Ordering::SeqCst) {
        start_event_bridge(app.clone(), port);
    }
}

/// Env var letting power users point the sidecar spawn at an explicit binary
/// path, bypassing name resolution entirely.
const SIDECAR_BIN_ENV: &str = "TRUSTY_AGENTS_SIDECAR_BIN";

/// Ordered list of binary names `resolve_tagent_binary` searches for.
///
/// Why: The crate's actual `[[bin]]` target (`crates/trusty-agents/Cargo.toml`)
/// is named `tagent` — `trusty-agents` is only the crate/directory name, and
/// no binary by that name is ever produced by `cargo build`/`cargo install`.
/// Searching for `trusty-agents` alone means `resolve_tagent_binary` can never
/// find a stock install, so `ensure_api_server` spawns a nonexistent path and
/// the desktop app fails with "subprocess exited with status Some(1)" on
/// first launch (part of epic #3052). `tagent` is tried first as the correct,
/// canonical name; `trusty-agents` is kept as a secondary candidate for
/// backward compatibility with any manual symlink/alias a user already
/// created as a workaround.
/// What: Pure, filesystem-free ordered candidate list — no I/O, so it is
/// trivially unit-testable independent of `$PATH`/`$HOME` state.
/// Test: `sidecar_candidate_names_prefers_tagent_over_legacy_name`.
fn sidecar_candidate_names() -> &'static [&'static str] {
    &["tagent", "trusty-agents"]
}

/// Find the sidecar binary. Honors an explicit `TRUSTY_AGENTS_SIDECAR_BIN`
/// override first, then searches `$PATH`, well-known install dirs, and the
/// sibling Cargo workspace's debug/release target — in that order, trying
/// each candidate name from `sidecar_candidate_names()` at every location —
/// so `cargo run` in this repo's root doesn't require a global install.
fn resolve_tagent_binary() -> std::path::PathBuf {
    // 0. Explicit override for power users pointing at a specific build.
    if let Ok(explicit) = std::env::var(SIDECAR_BIN_ENV) {
        if !explicit.is_empty() {
            return std::path::PathBuf::from(explicit);
        }
    }

    let candidates = sidecar_candidate_names();

    // 1. $PATH (works in dev / CLI contexts)
    for name in candidates {
        if let Ok(path) = which(name) {
            return path;
        }
    }
    // 2. Explicit well-known install locations (macOS .app bundles get a
    //    minimal PATH like `/usr/bin:/bin:/usr/sbin:/sbin` so $HOME/.cargo/bin
    //    and $HOME/.local/bin are invisible above). #364
    if let Ok(home) = std::env::var("HOME") {
        let home = std::path::Path::new(&home);
        for dir in [".cargo/bin", ".local/bin"] {
            for name in candidates {
                let candidate = home.join(dir).join(name);
                if candidate.is_file() {
                    return candidate;
                }
            }
        }
    }
    // 3. Sibling Cargo workspace target dir (ui/src-tauri → trusty-agents root).
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    if let Some(root) = manifest.ancestors().nth(2) {
        for profile in ["release", "debug"] {
            for name in candidates {
                let candidate = root.join("target").join(profile).join(name);
                if candidate.is_file() {
                    return candidate;
                }
            }
        }
    }
    // 4. Fallback: trust $PATH resolution at spawn time using the canonical name.
    std::path::PathBuf::from(candidates[0])
}

/// Minimal `which` that tolerates missing `which` crate dep.
fn which(name: &str) -> Result<std::path::PathBuf, ()> {
    let path = std::env::var_os("PATH").ok_or(())?;
    for entry in std::env::split_paths(&path) {
        let candidate = entry.join(name);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(())
}

/// Probe `/api/health`, returning `true` only when a sidecar THIS process owns
/// is answering on `port`.
///
/// Why: #3734 adoption race. A plain 200 is not enough. If a previous GUI died
/// without its sidecar being reaped, that orphan keeps answering on the fixed
/// port until its own parent-death watchdog fires (~2s). Both callers of this
/// function would otherwise trust it: `ensure_api_server`'s fast path would
/// ADOPT it (recording no child, so it can never respawn it) and `check_health`
/// would mark the app READY against it — either way the app goes dark the
/// instant the orphan self-exits, with no auto-heal. Gating on parentage makes
/// "healthy" mean "MY sidecar is up": `ensure_api_server` then spawns its own
/// and the boot loop's `ensure`-retry heals once the orphan releases the port.
/// What: Accepts a 200 only when the reported `ppid` equals our pid. If the
/// sidecar does not report `ppid` (older build, or non-unix where `getppid` is
/// absent), falls back to trusting the 200 so those setups still work.
/// Test: covered end-to-end by the desktop app; the parentage field it reads is
/// unit-tested server-side by `health_returns_ok_and_version`.
pub(crate) async fn http_health(port: u16) -> bool {
    let url = format!("{}/api/health", api_base(port));
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(500))
        .build();
    let Ok(client) = client else { return false };
    let Ok(resp) = client.get(&url).send().await else {
        return false;
    };
    if !resp.status().is_success() {
        return false;
    }
    let Ok(body) = resp.json::<serde_json::Value>().await else {
        // 200 but the body did not parse — trust liveness (defensive; the real
        // server always returns JSON here).
        return true;
    };
    match body.get("ppid").and_then(serde_json::Value::as_u64) {
        // Only OUR sidecar (parented to this GUI process) counts as healthy.
        Some(ppid) => ppid == u64::from(std::process::id()),
        // Sidecar didn't report parentage (older/non-unix) — trust the 200.
        None => true,
    }
}

/// Reap the spawned `trusty-agents --api` sidecar, exactly once.
///
/// Why: The tray "Quit" item and the real-quit `RunEvent::ExitRequested`
/// path (Cmd+Q, app-menu Quit) both need to kill the sidecar before the app
/// goes away, and they can race each other (the Quit item itself calls
/// `AppHandle::exit`, which raises a fresh `ExitRequested`). `guard.take()`
/// empties the slot on first use, so a second caller finds `None` and is a
/// safe no-op — no double-kill, no error.
/// What: Locks `state.child`, takes the `Child` if present, and hands it to
/// [`terminate_child`] for graceful-then-forced reaping. Releases the lock
/// *before* awaiting termination so a concurrent `ensure_api_server` isn't
/// blocked for the whole grace window.
/// Test: `terminate_child` is unit-tested against real child processes (see
/// the `tests` module); the quit wiring itself is smoke-tested manually —
/// quit via the tray item / Cmd+Q and confirm `ps` shows no `tagent --api`.
pub async fn kill_sidecar(state: &SharedApi) {
    // Take the child out under the lock, then drop the lock before awaiting so
    // the (up to `SIDECAR_GRACE`) termination doesn't hold off other callers.
    let child = {
        let mut guard = state.child.lock().await;
        guard.take()
    };
    if let Some(child) = child {
        terminate_child(child, SIDECAR_GRACE).await;
    }
}

/// Terminate a sidecar child gracefully, escalating to SIGKILL after `grace`,
/// and reap it so no zombie or orphan survives.
///
/// Why: `#3372` — the app quit path sent SIGKILL without awaiting the reap (or,
/// on the fast path, never awaited at all), so the `tagent --api` sidecar could
/// outlive the GUI and keep holding its fixed port. Repeated demo restarts then
/// accumulated orphans and port conflicts. We now (1) send SIGTERM so the
/// sidecar can unbind its listener cleanly, (2) wait up to `grace` for it to
/// exit, and (3) SIGKILL + reap if it's still alive. An already-exited child is
/// handled without panicking (`try_wait` reaps it and returns early).
/// What: Consumes the `Child`. Returns the observed `ExitStatus`, or `None`
/// only if the final reap itself errors (e.g. the child was already reaped
/// elsewhere). Pure process lifecycle — no Tauri/window state — so it is
/// unit-testable without a display server.
/// Test: `terminate_child_reaps_running_process`,
/// `terminate_child_handles_already_exited`.
pub(crate) async fn terminate_child(mut child: Child, grace: Duration) -> Option<ExitStatus> {
    // Already dead? Reap it (non-blocking) and we're done — no signal, no panic.
    if let Ok(Some(status)) = child.try_wait() {
        return Some(status);
    }

    // Graceful phase (unix): SIGTERM, then wait up to `grace` for a clean exit.
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        // SAFETY: `pid` names a child process we own and have not yet reaped;
        // `SIGTERM` is a valid signal. `kill` has no memory-safety effects — a
        // stale pid merely returns ESRCH, which we intentionally ignore.
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGTERM);
        }
        if let Ok(Ok(status)) = tokio::time::timeout(grace, child.wait()).await {
            return Some(status);
        }
        tracing::warn!(
            pid,
            ?grace,
            "sidecar did not exit on SIGTERM within grace; sending SIGKILL"
        );
    }

    // Force phase: SIGKILL and reap. Also the sole path on non-unix targets.
    let _ = child.start_kill();
    child.wait().await.ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Why: `resolve_tagent_binary` reads the process-global `TRUSTY_AGENTS_SIDECAR_BIN`
    // env var; this lock keeps the one test that mutates it from racing other
    // tests in this binary that also touch process env state (e.g.
    // `overlay.rs`'s `HOME`-mutating tests), matching the established
    // convention in `overlay.rs`.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Before this fix, resolution only ever searched for a binary literally
    /// named `trusty-agents` — which `cargo build`/`cargo install` never
    /// produces (the crate's `[[bin]]` target is `tagent`). This test pins
    /// the fixed ordering: `tagent` (the real, installable binary name) must
    /// be tried before the legacy `trusty-agents` fallback. Asserting on the
    /// pre-fix single-candidate behavior would have failed this exact
    /// assertion (`candidates[0] == "tagent"`), since the old code had no
    /// `tagent` candidate at all.
    #[test]
    fn sidecar_candidate_names_prefers_tagent_over_legacy_name() {
        let candidates = sidecar_candidate_names();
        assert_eq!(
            candidates.first().copied(),
            Some("tagent"),
            "the real, installable binary name must be the first candidate"
        );
        assert!(
            candidates.contains(&"trusty-agents"),
            "legacy name kept as a backward-compat fallback"
        );
    }

    /// `resolve_tagent_binary` must let power users bypass name resolution
    /// entirely via `TRUSTY_AGENTS_SIDECAR_BIN`, e.g. to point at a
    /// non-standard build. This exercises `resolve_tagent_binary` end to
    /// end rather than just the candidate list.
    #[test]
    fn resolve_tagent_binary_honors_explicit_override_env() {
        let _guard = ENV_LOCK.lock().expect("lock poisoned");
        let prev = std::env::var_os(SIDECAR_BIN_ENV);
        // SAFETY: guarded by ENV_LOCK; restored before returning.
        unsafe {
            std::env::set_var(SIDECAR_BIN_ENV, "/tmp/custom-tagent-build");
        }
        let resolved = resolve_tagent_binary();
        // SAFETY: guarded by ENV_LOCK.
        unsafe {
            match &prev {
                Some(v) => std::env::set_var(SIDECAR_BIN_ENV, v),
                None => std::env::remove_var(SIDECAR_BIN_ENV),
            }
        }
        assert_eq!(
            resolved,
            std::path::PathBuf::from("/tmp/custom-tagent-build")
        );
    }

    /// The #3372 regression guard: a running sidecar must actually be reaped by
    /// `terminate_child`, not merely signalled-and-forgotten. We spawn a real
    /// long-lived child (`sleep 300`), confirm it is alive, terminate it, and
    /// assert (a) we get an `ExitStatus` back (proof the reap `wait` completed)
    /// and (b) the OS no longer knows the pid — i.e. no orphan survives. Uses
    /// real OS processes, so it needs no display server and runs in CI.
    #[cfg(unix)]
    #[tokio::test]
    async fn terminate_child_reaps_running_process() {
        let child = tokio::process::Command::new("sleep")
            .arg("300")
            .spawn()
            .expect("spawn sleep");
        let pid = child.id().expect("running child has a pid") as libc::pid_t;

        // Sanity: the process is alive before we terminate it.
        // SAFETY: signal 0 performs existence/permission checks only, no delivery.
        assert_eq!(unsafe { libc::kill(pid, 0) }, 0, "child should be alive");

        let status = terminate_child(child, SIDECAR_GRACE).await;
        assert!(
            status.is_some(),
            "terminate_child must reap the child and yield an ExitStatus"
        );

        // The pid must be gone (reaped): kill(pid, 0) fails with ESRCH.
        // SAFETY: signal 0, existence check only.
        let rc = unsafe { libc::kill(pid, 0) };
        assert_eq!(rc, -1, "reaped child's pid must no longer exist");
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH),
            "kill on the reaped pid must fail with ESRCH (no such process)"
        );
    }

    /// An already-exited child must be handled without panicking or hanging —
    /// `terminate_child` should reap it via the fast `try_wait` path and return
    /// its status rather than signalling a dead pid or blocking on `wait`.
    #[cfg(unix)]
    #[tokio::test]
    async fn terminate_child_handles_already_exited() {
        let mut child = tokio::process::Command::new("true")
            .spawn()
            .expect("spawn true");
        // Let it run to completion, but do NOT reap it here — leave that to
        // `terminate_child` so we exercise the already-exited branch. Bound the
        // wait with a hard deadline so a wedged `true` fails loudly instead of
        // hanging the test runner (condition-based-waiting).
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "`true` did not exit within 5s"
                    );
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Err(e) => panic!("try_wait failed: {e}"),
            }
        }

        let status = terminate_child(child, SIDECAR_GRACE).await;
        assert!(
            status.is_some(),
            "an already-exited child must be reaped, not left hanging"
        );
    }

    /// Pins the SIGKILL-escalation branch: a child that *ignores* SIGTERM must
    /// still be forcibly reaped after the grace window elapses. We spawn a
    /// `python3` that installs `SIG_IGN` for SIGTERM and only then touches a
    /// marker file — we wait for that marker before signalling, otherwise the
    /// SIGTERM could race process startup and land before the handler is
    /// installed (which would let the graceful phase succeed and never exercise
    /// escalation). We assert (a) at least `grace` elapsed — proof we waited out
    /// the graceful window rather than killing immediately, (b) the child died
    /// from signal 9 (SIGKILL), and (c) its pid is gone. Skips cleanly if
    /// `python3` isn't on PATH.
    #[cfg(unix)]
    #[tokio::test]
    async fn terminate_child_escalates_to_sigkill_when_sigterm_ignored() {
        use std::os::unix::process::ExitStatusExt;

        // Unique marker path this child touches once its SIGTERM handler is in.
        let marker = std::env::temp_dir().join(format!(
            "trusty-3372-sigterm-ignore-{}-{}.marker",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock before epoch")
                .as_nanos()
        ));
        // Best-effort clean slate in case a previous run left one behind.
        let _ = std::fs::remove_file(&marker);

        let spawn = tokio::process::Command::new("python3")
            .arg("-c")
            .arg(
                "import signal,sys,time\n\
                 signal.signal(signal.SIGTERM, signal.SIG_IGN)\n\
                 open(sys.argv[1], 'w').close()\n\
                 time.sleep(300)",
            )
            .arg(&marker)
            .spawn();
        let child = match spawn {
            Ok(c) => c,
            Err(_) => {
                eprintln!("skipping: python3 not available on PATH");
                return;
            }
        };
        let pid = child.id().expect("running child has a pid") as libc::pid_t;

        // Wait (bounded) until the child has installed its SIGTERM handler.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !marker.exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "child never signalled SIGTERM-handler readiness within 5s"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        let grace = Duration::from_millis(200);
        let start = std::time::Instant::now();
        let status = terminate_child(child, grace).await;
        let elapsed = start.elapsed();
        let _ = std::fs::remove_file(&marker);

        assert!(
            elapsed >= grace,
            "must wait out the full grace window before SIGKILL (elapsed {elapsed:?} < {grace:?})"
        );
        let status = status.expect("child must be reaped and yield an ExitStatus");
        assert_eq!(
            status.signal(),
            Some(libc::SIGKILL),
            "a SIGTERM-ignoring child must be killed by SIGKILL (signal 9)"
        );

        // The pid must be gone (reaped): kill(pid, 0) fails with ESRCH.
        // SAFETY: signal 0, existence check only.
        let rc = unsafe { libc::kill(pid, 0) };
        assert_eq!(rc, -1, "reaped child's pid must no longer exist");
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH),
            "kill on the reaped pid must fail with ESRCH (no such process)"
        );
    }
}
