//! Shared test-only synchronization primitives and executable-file helpers.
//!
//! Why: Multiple unit tests across different modules (`init::tests`, etc.)
//! sandbox `$HOME` with `std::env::set_var` to redirect file I/O into a
//! tempdir. `set_var` is a process-wide mutation and `cargo test` runs unit
//! tests on a multi-threaded executor by default, so two concurrent tests
//! sandboxing HOME stomp on each other and one will observe the other's
//! tempdir (or restore HOME mid-flight). The classic fix is a per-test-module
//! `static Mutex`, but that only serializes tests WITHIN one module —
//! cross-module races (e.g. `init::seed_skills_*` vs another module's
//! HOME-mutating test) still flake, and a test that skips the lock races
//! everyone. The durable fix is to inject the path instead of mutating HOME
//! (see `mistake_log`'s `record_at` seam, #2709); `HOME_LOCK` remains for
//! tests not yet migrated to injection.
//! What: Exposes a single process-wide `HOME_LOCK` that every test mutating
//! `$HOME` must hold. Also provides `write_executable_script` and
//! `spawn_script` helpers to avoid ETXTBSY races (#1528).
//! Compiled only under `#[cfg(test)]` so it costs nothing in release builds.
//! Test: Used by `init::tests::*` and other modules that still sandbox HOME.
//! Such tests were order-dependent before this lock — passing in isolation but
//! failing under `cargo test`.

#![cfg(test)]

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Process-wide mutex serializing tests that mutate `$HOME`.
///
/// Why: `std::env::set_var` is a process-global mutation; tokio's default
/// multi-threaded test runtime causes interleaved tests to overwrite each
/// other's HOME before they restore the original value. A single static
/// Mutex shared across all modules keeps such tests sequential without
/// forcing `--test-threads=1` for the whole crate.
/// What: Use `let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());`
/// at the top of any test that calls `std::env::set_var("HOME", ...)`.
/// `unwrap_or_else(into_inner)` ensures a panic in one test doesn't poison
/// the lock for siblings.
pub static HOME_LOCK: Mutex<()> = Mutex::new(());

/// Process-wide mutex serializing tests that mutate LLM credential env vars
/// (#250). Same rationale as `HOME_LOCK` — `std::env::set_var` is global so
/// concurrent credential-routing tests would race.
pub static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Force `trusty_common::inference::credentials::load_env_local_once` to
/// have already run in this process, before any test-local `remove_var`.
///
/// Why (#3464): that loader is a process-global `OnceLock` — it fires at
/// most once per test binary, on whichever call (from ANY test, in ANY
/// module) happens to be first, and folds `.env.local` content into the
/// process environment (searched upward from cwd — which, for this repo's
/// own linked-worktree layout, resolves all the way up to the shared main
/// checkout root — AND from `$HOME`). A credential test that clears a few
/// env vars and then calls production code that transitively calls
/// `resolve_key` implicitly assumes that call is a no-op; if it is instead
/// the FIRST call in the whole process, the loader can silently re-populate
/// a var the test just removed, in the middle of the very function under
/// test, purely because a `.env.local` happens to exist on that machine (a
/// real, gitignored dev file — confirmed live as the cause of #3464's
/// `other_configured_providers_empty_when_nothing_else_configured` flake).
/// Calling this FIRST, before any `remove_var`, guarantees the one-time load
/// has already happened (whether it was this call or an earlier test's), so
/// every subsequent `remove_var` in the same locked critical section is the
/// last word — no future call in this process can ever repopulate it.
/// What: thin wrapper over `load_env_local_once()` (itself idempotent).
/// Callers should hold `ENV_LOCK` (and `HOME_LOCK` if they also sandbox
/// `$HOME`) for their entire body, same convention as every other
/// env-mutating test; async tests that cannot hold a `std::sync::Mutex`
/// guard across `.await` may call this un-guarded and rely on `#[serial]`
/// instead, matching the existing convention for those tests.
/// Test: exercised indirectly via every test that now calls this before
/// clearing credential env vars (`llm::credentials::tests::clear_all`,
/// `llm::helpers::tests::create_client_*`, `llm::adapter::tests::*_when_env_absent`,
/// `llm::http::tests::send_raw_completion_*`, `ctrl::ctrl_turn::dispatch::tests::clear_creds_env`,
/// `runtime::cli_def::tests::credential_banner::clear_all`) — not
/// independently unit tested since it depends on real process/filesystem
/// state, exactly like `load_env_local_once` itself.
pub fn force_env_local_loaded() {
    trusty_common::inference::credentials::load_env_local_once();
}

/// Clear the process env var for every provider the shared inference
/// registry maps to a credential env var, plus `CLAUDE_CODE_OAUTH_TOKEN`
/// (ctrl/PM-only, not in the registry).
///
/// Why (#3464): `llm::credentials::other_configured_providers()` checks
/// EVERY registry provider (`fireworks`, `atlascloud`, `openai`, `together`,
/// ...), not just the three `openrouter`/`anthropic`/`claude-code` names
/// `pick_credentials` cares about. A test asserting "nothing else is
/// configured" must clear all of them, not a hand-picked subset — otherwise
/// it silently depends on the local machine (or CI runner) never having any
/// of those provider credentials configured anywhere resolvable (env,
/// `.env.local`, or the secure store). Must be called AFTER
/// [`force_env_local_loaded`], never before — see that function's docs.
/// What: iterates `trusty_common::inference::registry::all()`, removing
/// each provider's `env_var_for` mapping when one exists, then removes
/// `CLAUDE_CODE_OAUTH_TOKEN` explicitly (it has no registry entry — it's
/// ctrl/PM's own OAuth routing credential, not an inference provider).
/// Test: `llm::credentials::tests::other_configured_providers_empty_when_nothing_else_configured`.
pub fn clear_all_credential_env_vars() {
    for caps in trusty_common::inference::registry::all() {
        if let Some(var) = trusty_common::inference::credentials::env_var_for(caps.id.as_str()) {
            // SAFETY: caller holds `ENV_LOCK` for the whole body (documented
            // requirement, same convention as every other env mutation in
            // this module).
            unsafe {
                std::env::remove_var(var);
            }
        }
    }
    // SAFETY: see above.
    unsafe {
        std::env::remove_var("CLAUDE_CODE_OAUTH_TOKEN");
    }
}

/// Write a shell script to `dir/name`, flush and close the file handle, THEN
/// set the execute bit — eliminating the ETXTBSY race (#1528).
///
/// Why: The Linux kernel rejects `execve` with ETXTBSY (os error 26) when
/// ANY process holds an open writable fd to the target inode — including the
/// writing process itself. Closing/dropping the `File` before `set_permissions`
/// and before `spawn` removes that fd, satisfying the kernel's constraint.
/// The safe pattern is: open → write → flush → **drop the `File`** → chmod
/// → spawn. Without the explicit drop, the writable fd can still be open when
/// `execve` is called, triggering ETXTBSY even under light load.
/// What: Creates the file at `dir.join(name)`, writes `contents`, syncs,
/// drops the file handle, sets mode 0o755, and returns the absolute path.
/// Test: All tests in `subprocess::tests` and `agents::claude_code_runner::tests`
/// that spawn a mock shell script call this helper; ETXTBSY flakiness should
/// not recur after the refactor.
#[cfg(unix)]
pub fn write_executable_script(dir: &Path, name: &str, contents: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;

    let path = dir.join(name);
    {
        // Scope the File so it is closed before set_permissions.
        let mut f = std::fs::File::create(&path)
            .unwrap_or_else(|e| panic!("create {}: {e}", path.display()));
        f.write_all(contents.as_bytes())
            .unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
        f.flush()
            .unwrap_or_else(|e| panic!("flush {}: {e}", path.display()));
        // `f` drops here — fd is closed before chmod.
    }
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .unwrap_or_else(|e| panic!("chmod {}: {e}", path.display()));
    path
}

/// Spawn a shell script at `path` with stdin=null, stdout=piped, stderr=inherit,
/// retrying on ETXTBSY (os error 26) up to 3 times with exponential back-off.
///
/// Why: Even after closing the write handle before chmod, kernel page-cache
/// flush timing under heavy CI load can delay the execute permission becoming
/// visible to `execve`. A small bounded retry (≤3 attempts, 5 ms back-off)
/// acts as belt-and-suspenders without hiding real failures.
/// What: Constructs a `tokio::process::Command` fresh on each attempt
/// (required because `Command` is not `Clone`) and calls `.spawn()`.
/// On ETXTBSY it backs off and retries; on any other error or after
/// `MAX_ATTEMPTS` ETXTBSY hits it returns the error immediately.
/// The stdio setup (stdin null / stdout piped / stderr inherited) matches
/// the pattern used by all subprocess and claude-code-runner tests.
/// Test: Covered indirectly by every async test that calls `spawn_script`;
/// a direct unit test would require artificially injecting ETXTBSY which is
/// impractical in pure Rust tests. The helper's correctness is validated by
/// the absence of CI failures after the refactor.
#[cfg(unix)]
pub async fn spawn_script(path: &std::path::Path) -> std::io::Result<tokio::process::Child> {
    use std::process::Stdio;

    const MAX_ATTEMPTS: u32 = 3;
    const BACKOFF_MS: u64 = 5;

    let mut last_err: Option<std::io::Error> = None;
    for attempt in 0..MAX_ATTEMPTS {
        let mut cmd = tokio::process::Command::new(path);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        match cmd.spawn() {
            Ok(child) => return Ok(child),
            Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                last_err = Some(e);
                tokio::time::sleep(std::time::Duration::from_millis(
                    BACKOFF_MS * (1 << attempt),
                ))
                .await;
            }
            Err(e) => return Err(e),
        }
    }
    Err(last_err.unwrap())
}
