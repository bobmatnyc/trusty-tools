//! End-to-end isolation tests for issue #880.
//!
//! Why: when `TRUSTY_DATA_DIR_OVERRIDE` is set (test rigs, CI, parallel runs)
//! the daemon used to leak into the real environment in two ways:
//! (a) Writing the isolated instance's address to `~/.trusty-memory/http_addr`,
//!     overwriting the real production daemon's discovery dotfile.
//! (b) The startup pin-scan reading `~/Projects`, `~/Developer`, … and
//!     importing palaces from the live system into the isolated data root.
//!
//! Both paths are now guarded by `is_data_dir_override_active()`. These tests
//! prove the guards work end-to-end by spawning the real binary under an
//! overridden data dir and asserting:
//! (a) `~/.trusty-memory/http_addr` is not modified (same mtime + content).
//! (b) The override data root contains no palace directory that could only
//!     have come from the real environment's pin-scan (e.g. `cto/`).
//!
//! What: each test spawns `trusty-memory serve --foreground --http 127.0.0.1:0`
//! with `TRUSTY_DATA_DIR_OVERRIDE` pointing at an isolated temp directory.
//! Rather than sleeping a fixed duration the harness polls for the override
//! data root's `http_addr` file (written synchronously by the daemon as part
//! of `run_http_on`), giving up after a generous timeout. Once the file
//! appears the daemon is killed. Post-conditions are then asserted against the
//! real filesystem state.
//!
//! Test: `cargo test -p trusty-memory --test isolation_override`.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

// ---------------------------------------------------------------------------
// Process-wide lock for tests that mutate TRUSTY_DATA_DIR_OVERRIDE.
//
// Why: `std::env::set_var` / `remove_var` mutate process-wide state; running
// concurrent tests that each set the same env var produces non-deterministic
// results. The integration test runner spawns multiple tests in parallel by
// default, so tests that manipulate the override env var must serialise.
// ---------------------------------------------------------------------------
fn env_lock() -> &'static Mutex<()> {
    use std::sync::OnceLock;
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

// ---------------------------------------------------------------------------
// Poll-with-timeout parameters for daemon boot detection.
//
// Why: the dotfile write and the http_addr file write complete synchronously
// inside `run_http_on` before the first connection is accepted, so polling
// for the http_addr file is a reliable readiness signal. Polling eliminates
// both the flakiness of a fixed sleep on slow CI hardware and the wasted
// time on fast machines (typical boot is < 500 ms).
// ---------------------------------------------------------------------------

/// How often to check for the daemon's readiness file.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Maximum time to wait for the daemon to write its readiness file.
///
/// Why: 30 s covers even the most resource-constrained CI runners without
/// blocking a fast local machine longer than necessary; on typical dev
/// hardware the daemon is ready in < 1 s.
const BOOT_TIMEOUT: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Locate the `trusty-memory` binary produced by Cargo for this test harness.
fn locate_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_trusty-memory"))
}

/// Resolve `~/.trusty-memory/http_addr` — the legacy dotfile path.
///
/// Why: we need to check this path both before and after boot to verify the
/// isolated instance did not overwrite it.
/// What: returns `$HOME/.trusty-memory/http_addr` using `dirs::home_dir`, or
/// `None` if `$HOME` is not available (unusual in practice — the test will
/// be skipped if `None` is returned).
fn dotfile_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".trusty-memory").join("http_addr"))
}

/// Capture the mtime + contents of a file, or `None` if it does not exist.
fn snapshot(path: &Path) -> Option<(SystemTime, String)> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta.modified().ok()?;
    let content = std::fs::read_to_string(path).ok()?;
    Some((mtime, content))
}

/// Poll `path` until it exists or `BOOT_TIMEOUT` elapses.
///
/// Why: the daemon writes its `http_addr` file synchronously during startup
/// (`run_http_on`), so waiting for that file to appear is a reliable,
/// latency-efficient readiness signal — no fixed sleep needed.
/// What: wakes every `POLL_INTERVAL` and calls `Path::exists`; returns `true`
/// once the file appears, `false` if `BOOT_TIMEOUT` elapses first.
/// Test: used by `boot_isolated` and `isolated_instance_does_not_overwrite_dotfile`.
fn wait_for_file(path: &Path) -> bool {
    let deadline = std::time::Instant::now() + BOOT_TIMEOUT;
    loop {
        if path.exists() {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// Spawn `trusty-memory serve --foreground --http 127.0.0.1:0` with an
/// isolated data dir, wait for the override data root's `http_addr` file to
/// appear (indicating the startup sequence completed), then kill the process.
///
/// Why: `--foreground` prevents the binary from self-forking (plain `serve`
/// daemonises and the parent exits 0, which races our kill). `--http
/// 127.0.0.1:0` lets the OS pick a free port so concurrent test runs cannot
/// collide. `TRUSTY_DATA_DIR_OVERRIDE` points at the temp dir so every data
/// write (http_addr file, palaces) lands inside the isolated root.
///
/// Why poll instead of sleep: a fixed sleep is simultaneously flaky on slow
/// CI (the daemon may not have finished its startup sequence) and wasteful on
/// fast machines (typical startup is << 2 s). Polling for the `http_addr`
/// file — which the daemon writes synchronously as part of `run_http_on`,
/// before accepting the first connection — gives us an exact readiness signal
/// with no wasted time.
///
/// Ordering note (TOCTOU): this helper polls for
/// `<override_base>/trusty-memory/http_addr` as its readiness signal. The
/// daemon writes that file synchronously inside `run_http_on`, which runs
/// AFTER the dotfile-write guard and pin-scan have already completed. If
/// future refactors move the `http_addr` write earlier in the startup
/// sequence (e.g. before the pin-scan), callers that rely on "http_addr
/// present ⟹ pin-scan done" must update this readiness signal accordingly.
///
/// What: spawns the child with piped stdio so it produces no console noise,
/// polls until `<override_base>/trusty-memory/http_addr` exists or
/// `BOOT_TIMEOUT` elapses, kills the child, reaps it via `wait`. Panics if
/// the readiness file does not appear within the timeout.
fn boot_isolated(override_base: &Path) {
    let bin = locate_binary();
    let mut child = Command::new(&bin)
        .arg("serve")
        .arg("--foreground")
        .arg("--http")
        .arg("127.0.0.1:0")
        .env("TRUSTY_DATA_DIR_OVERRIDE", override_base)
        // Suppress the startup pin-scan eprintln! so test output is clean.
        .env("RUST_LOG", "error")
        // Needed to prevent palace-slug enforcement from requiring a real
        // project root.
        .env("TRUSTY_SKIP_PALACE_ENFORCEMENT", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn trusty-memory binary");

    // Poll for the readiness file rather than sleeping a fixed duration.
    // The daemon writes `<override_base>/trusty-memory/http_addr`
    // synchronously during `run_http_on` — its appearance signals that
    // the dotfile write and pin-scan have both already completed.
    let readiness_file = override_base.join("trusty-memory").join("http_addr");
    let ready = wait_for_file(&readiness_file);

    let _ = child.kill();
    let _ = child.wait();

    assert!(
        ready,
        "trusty-memory did not write its http_addr file inside the override data root \
         within {:.0?}.\nExpected path: {}\nCheck that the binary starts correctly and \
         that TRUSTY_DATA_DIR_OVERRIDE is honoured.",
        BOOT_TIMEOUT,
        readiness_file.display()
    );
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Why (issue #880 — dotfile leak): when `TRUSTY_DATA_DIR_OVERRIDE` is set
/// the daemon must NOT write to `~/.trusty-memory/http_addr`. The real
/// production daemon's discovery dotfile must remain untouched.
///
/// What: snapshots the mtime and content of `~/.trusty-memory/http_addr`
/// before booting an isolated daemon, boots it (waiting for the override
/// data root's `http_addr` to appear to confirm startup completed), then
/// asserts the dotfile is either still absent (was absent before) or has the
/// identical mtime and content as before (no write occurred). Also asserts
/// that the override data root's own `http_addr` file IS present (the
/// isolated instance must write it inside the override dir).
/// Test: this test. Skipped when `$HOME/.trusty-memory/http_addr` cannot be
/// resolved (unusual locked-down environment).
#[test]
fn isolated_instance_does_not_overwrite_dotfile() {
    // Acquire the process-wide env lock. This test does not itself mutate
    // TRUSTY_DATA_DIR_OVERRIDE, but the unit tests in this same binary do.
    // Holding the lock serialises execution so a concurrent unit test cannot
    // transiently clear the env var between the child's fork and its own
    // `is_data_dir_override_active()` check, which would cause the child
    // to take the production code path and write to the dotfile.
    let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());

    let dotfile = match dotfile_path() {
        Some(p) => p,
        None => {
            eprintln!("SKIP: could not resolve $HOME — skipping dotfile isolation test");
            return;
        }
    };

    // Record the pre-boot snapshot.
    let before = snapshot(&dotfile);

    // Boot an isolated daemon pointing at a fresh temp dir. `boot_isolated`
    // polls until the override data root's http_addr appears, so by the
    // time it returns the startup sequence (including any dotfile write
    // attempt) has completed.
    let tmp = tempfile::tempdir().expect("tempdir");
    boot_isolated(tmp.path());

    // Post-boot check.
    let after = snapshot(&dotfile);

    match (before, after) {
        // File did not exist before — it must still not exist after.
        (None, None) => {
            // Correct: override instance did not create the dotfile.
        }
        (None, Some((_, content))) => {
            panic!(
                "dotfile did not exist before the isolated boot but was created afterwards.\n\
                 Content: {content:?}\n\
                 The isolated daemon must not write to ~/.trusty-memory/http_addr."
            );
        }
        // File existed before — it must be identical after (same mtime proves
        // no write occurred; same content is a belt-and-suspenders check).
        (Some((mtime_before, content_before)), Some((mtime_after, content_after))) => {
            assert_eq!(
                mtime_before, mtime_after,
                "~/.trusty-memory/http_addr mtime changed after isolated daemon boot — \
                 the override instance must not write to the production dotfile.\n\
                 content before: {content_before:?}\n\
                 content after:  {content_after:?}"
            );
            assert_eq!(
                content_before, content_after,
                "~/.trusty-memory/http_addr content changed after isolated daemon boot — \
                 the override instance must not write to the production dotfile."
            );
        }
        // File existed before but vanished after — that's a separate bug,
        // not the dotfile-overwrite we're guarding against. Flag it clearly.
        (Some((_mtime_before, content_before)), None) => {
            // This would be odd — it means the dotfile was *deleted*. We do
            // not gate on this case; it's out of scope for this test. Just
            // note it for debugging.
            eprintln!(
                "NOTE: ~/.trusty-memory/http_addr was present before the test but missing \
                 after (content was: {content_before:?}). This may indicate a concurrent \
                 production daemon restart; the dotfile-leak guard itself is not triggered."
            );
        }
    }

    // Separately, assert the override data root's http_addr file IS present.
    // `boot_isolated` already polled for this file and would have panicked
    // above if it were absent; this assertion is a belt-and-suspenders
    // confirmation that the file persisted past the kill.
    let override_addr_file = tmp.path().join("trusty-memory").join("http_addr");
    assert!(
        override_addr_file.exists(),
        "isolated instance must write its http_addr file inside the override data root at \
         {}; file not found",
        override_addr_file.display()
    );
}

/// Why (issue #880 — pin-scan leak): when `TRUSTY_DATA_DIR_OVERRIDE` is set
/// the startup pin-scan must NOT walk the real `~/Projects`, `~/Developer`,
/// etc. Doing so would import palaces from the live system into the isolated
/// data root, defeating isolation and polluting the isolated palace registry.
///
/// What: boots an isolated daemon with an empty data dir (no pin files, no
/// pre-seeded palaces). After the boot we list every directory inside the
/// isolated `trusty-memory/` subdirectory. Because the temp dir starts
/// completely empty (no pin files, no project roots), a correctly isolated
/// daemon has nothing to scan and nothing to seed — the palace registry must
/// contain ZERO palaces. Any non-zero count is conclusive evidence that the
/// startup pin-scan or some other seeding path leaked through the isolation
/// guard. The assertion is self-contained: it does not depend on the names
/// of real-environment palaces the developer happens to have installed.
///
/// Why zero-count instead of named-marker assertions: the previous approach
/// used a hard-coded list of developer-specific palace names (`cto`, `duetto`,
/// etc.) which caused the test to be brittle — it passed on machines where
/// those projects did not exist, but would fail on any machine that happened
/// to have a pin file for an unlisted project. A zero-count assertion is
/// correct for any machine and any developer because the temp dir is
/// verifiably empty before the daemon boots.
/// Test: this test.
#[test]
fn isolated_instance_does_not_import_real_env_palaces() {
    // Acquire the process-wide env lock before spawning the child. This test
    // does not mutate TRUSTY_DATA_DIR_OVERRIDE itself, but the unit tests in
    // this same binary do. Holding the lock serialises execution so a
    // concurrent unit test cannot transiently clear the env var between the
    // child's fork and its `is_data_dir_override_active()` check, which would
    // cause the child to take the production code path and walk real env paths.
    let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());

    let tmp = tempfile::tempdir().expect("tempdir");

    // Boot an isolated daemon against the empty temp dir.
    boot_isolated(tmp.path());

    // The daemon's data root is `<override>/trusty-memory/`.
    let data_root = tmp.path().join("trusty-memory");

    // Collect every subdirectory of the data root — each one is a palace.
    // The `palaces/` subdir layout is also a valid layout; check both.
    let palace_dirs = collect_palace_dirs(&data_root);

    // The temp dir was empty when the daemon booted — no pin files exist, so
    // the pin-scan has nothing to discover, and no default seeding should
    // occur for an isolated instance. The isolated palace registry must be
    // empty. Any palace found here can only have come from the real
    // environment leaking through the isolation guard.
    assert!(
        palace_dirs.is_empty(),
        "Isolated instance created palace directories inside the override data root, \
         but the temp dir started empty — the startup pin-scan must not import palaces \
         from the real environment, and no default seeding should occur for an isolated \
         instance.\n\
         Override root: {}\n\
         Palaces found: {palace_dirs:?}",
        tmp.path().display()
    );
}

/// Collect every subdirectory under `data_root` that looks like a palace
/// directory (i.e. contains a `palace.json` file), or the directories inside
/// `data_root/palaces/` if that subdir exists.
///
/// Why: the trusty-memory daemon supports two palace-registry layouts:
/// - Flat: `<data_root>/<palace_id>/palace.json`
/// - Nested: `<data_root>/palaces/<palace_id>/palace.json`
///
/// We check both so the test is not fooled by layout differences.
fn collect_palace_dirs(data_root: &Path) -> Vec<PathBuf> {
    let mut result = Vec::new();

    // Try the nested layout first.
    let nested_root = data_root.join("palaces");
    if nested_root.is_dir() {
        append_palace_dirs(&nested_root, &mut result);
    }

    // Also try the flat layout (entries directly under data_root).
    append_palace_dirs(data_root, &mut result);

    // Deduplicate in case both layouts overlap (shouldn't happen, but safe).
    result.sort();
    result.dedup();
    result
}

/// Scan `registry_root` one level deep; add every subdirectory that contains
/// a `palace.json` to `out`.
fn append_palace_dirs(registry_root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(registry_root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() && path.join("palace.json").exists() {
            out.push(path);
        }
    }
}

// ---------------------------------------------------------------------------
// Unit tests for `is_data_dir_override_active` (issue #880)
// ---------------------------------------------------------------------------

/// Why (issue #880): the guard function must return `true` when the override
/// env var is set to a non-empty path so callers know to suppress the dotfile
/// write and pin-scan.
/// What: set `TRUSTY_DATA_DIR_OVERRIDE` to a non-empty string, call the
/// function, assert it returns `true`, then restore the env var.
/// Test: this test.
#[test]
fn is_data_dir_override_active_when_set() {
    let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    // SAFETY: test-only env mutation; serialised by env_lock().
    unsafe { std::env::set_var(trusty_common::DATA_DIR_OVERRIDE_ENV, "/tmp/some-override") };
    let result = trusty_memory::is_data_dir_override_active();
    unsafe { std::env::remove_var(trusty_common::DATA_DIR_OVERRIDE_ENV) };
    assert!(
        result,
        "is_data_dir_override_active must return true when env var contains a non-empty path"
    );
}

/// Why (issue #880): the guard must return `false` when the override env var
/// is not set, so the production daemon path (dotfile + pin scan) is
/// unaffected.
/// What: ensure `TRUSTY_DATA_DIR_OVERRIDE` is unset, call the function, assert
/// it returns `false`.
/// Test: this test.
#[test]
fn is_data_dir_override_inactive_when_unset() {
    let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    unsafe { std::env::remove_var(trusty_common::DATA_DIR_OVERRIDE_ENV) };
    let result = trusty_memory::is_data_dir_override_active();
    assert!(
        !result,
        "is_data_dir_override_active must return false when env var is unset"
    );
}

/// Why (issue #880): an accidentally blank env var (set to whitespace only)
/// must be treated as unset so a misconfigured environment does not suppress
/// the production dotfile write.
/// What: set `TRUSTY_DATA_DIR_OVERRIDE` to whitespace-only (`"   "`), call
/// the function, assert it returns `false`.
/// Test: this test.
#[test]
fn is_data_dir_override_inactive_when_blank() {
    let _guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    unsafe { std::env::set_var(trusty_common::DATA_DIR_OVERRIDE_ENV, "   ") };
    let result = trusty_memory::is_data_dir_override_active();
    unsafe { std::env::remove_var(trusty_common::DATA_DIR_OVERRIDE_ENV) };
    assert!(
        !result,
        "is_data_dir_override_active must return false when env var is blank/whitespace-only"
    );
}
