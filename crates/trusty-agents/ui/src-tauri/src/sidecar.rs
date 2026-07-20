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
//! `http_health`, `kill_sidecar`.
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

use std::sync::Arc;

use tauri::State;
use tokio::process::Child;
use tokio::sync::Mutex;

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
pub async fn ensure_api_server(port: u16, state: State<'_, SharedApi>) -> Result<(), String> {
    // Fast path: if the server already answers, there is nothing to do.
    if http_health(port).await {
        let mut p = state.port.lock().await;
        *p = Some(port);
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

    let child = tokio::process::Command::new(&binary)
        .arg("--api")
        .arg("--port")
        .arg(port.to_string())
        .env("TAGENT_PROJECT_DIR", &project_root)
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .map_err(|e| format!("failed to spawn {}: {e}", binary.display()))?;

    *guard = Some(child);
    let mut p = state.port.lock().await;
    *p = Some(port);
    Ok(())
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

pub(crate) async fn http_health(port: u16) -> bool {
    let url = format!("{}/api/health", api_base(port));
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(500))
        .build();
    let Ok(client) = client else { return false };
    match client.get(&url).send().await {
        Ok(r) => r.status().is_success(),
        Err(_) => false,
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
/// What: Locks `state.child`, takes the `Child` if present, sends it a kill
/// signal and awaits reaping.
/// Test: Manually — quit via the tray item, confirm the sidecar process
/// exits (`ps` shows no `trusty-agents --api`); quit via Cmd+Q, same check.
pub async fn kill_sidecar(state: &SharedApi) {
    let mut guard = state.child.lock().await;
    if let Some(mut child) = guard.take() {
        let _ = child.start_kill();
        let _ = child.wait().await;
    }
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
}
