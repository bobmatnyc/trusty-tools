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
//! The binary runs long enough to complete its startup sequence (dotfile write
//! + pin scan happen synchronously before the first request), then is killed.
//!
//! Post-conditions are asserted against the real filesystem state.
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

/// How long to wait for the daemon's startup sequence before killing it.
///
/// Why: both the dotfile write and the pin scan complete synchronously inside
/// `run_http_on` (dotfile) and `spawn_startup_tasks` (pin scan) before the
/// first connection is accepted. 2 seconds covers slow CI hardware; the pin
/// scan on a real developer machine typically finishes in < 100 ms.
const BOOT_WAIT: Duration = Duration::from_millis(2000);

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

/// Spawn `trusty-memory serve --foreground --http 127.0.0.1:0` with an
/// isolated data dir, wait for startup to complete, then kill the process.
///
/// Why: `--foreground` prevents the binary from self-forking (plain `serve`
/// daemonises and the parent exits 0, which races our kill). `--http
/// 127.0.0.1:0` lets the OS pick a free port so concurrent test runs cannot
/// collide. `TRUSTY_DATA_DIR_OVERRIDE` points at the temp dir so every data
/// write (http_addr file, palaces) lands inside the isolated root.
///
/// What: spawns the child with piped stdio so it produces no console noise,
/// sleeps BOOT_WAIT, kills the child, reaps it via `wait`.
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
    std::thread::sleep(BOOT_WAIT);
    let _ = child.kill();
    let _ = child.wait();
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Why (issue #880 — dotfile leak): when `TRUSTY_DATA_DIR_OVERRIDE` is set
/// the daemon must NOT write to `~/.trusty-memory/http_addr`. The real
/// production daemon's discovery dotfile must remain untouched.
///
/// What: snapshots the mtime and content of `~/.trusty-memory/http_addr`
/// before booting an isolated daemon, boots it, then asserts the dotfile is
/// either still absent (was absent before) or has the identical mtime and
/// content as before (no write occurred).
/// Test: this test. Skipped when `$HOME/.trusty-memory/http_addr` cannot be
/// resolved (unusual locked-down environment).
#[test]
fn isolated_instance_does_not_overwrite_dotfile() {
    let dotfile = match dotfile_path() {
        Some(p) => p,
        None => {
            eprintln!("SKIP: could not resolve $HOME — skipping dotfile isolation test");
            return;
        }
    };

    // Record the pre-boot snapshot.
    let before = snapshot(&dotfile);

    // Boot an isolated daemon pointing at a fresh temp dir.
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

    // Separately, assert the override data root's http_addr file IS present
    // (the isolated instance must write it inside the override dir).
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
/// isolated `trusty-memory/` subdirectory — every directory found is a palace.
/// We assert that no palace whose name starts with `cto` (or any other
/// name that could only originate from the real environment's pin-scan) was
/// created inside the isolated root.
///
/// The test is purposely conservative: it does not try to enumerate every
/// palace the real environment might have; instead it asserts that the palace
/// count is ≤ 1 (only the default "User Memories" / `default` palace is
/// allowed — it is seeded by the boot-time migration and is inherent to any
/// fresh data root, regardless of the pin scan).
/// Test: this test.
#[test]
fn isolated_instance_does_not_import_real_env_palaces() {
    let tmp = tempfile::tempdir().expect("tempdir");

    // Boot an isolated daemon against the empty temp dir.
    boot_isolated(tmp.path());

    // The daemon's data root is `<override>/trusty-memory/`.
    let data_root = tmp.path().join("trusty-memory");

    // Collect every subdirectory of the data root — each one is a palace.
    // The `palaces/` subdir layout is also a valid layout; check both.
    let palace_dirs = collect_palace_dirs(&data_root);

    // The real environment may have a `cto` palace. If the pin-scan leaked
    // through, we'd see a `cto/` directory inside the isolated root.
    let has_cto = palace_dirs.iter().any(|p| {
        p.file_name()
            .and_then(|n| n.to_str())
            .map(|s| s.starts_with("cto"))
            .unwrap_or(false)
    });
    assert!(
        !has_cto,
        "Isolated instance created a 'cto' palace inside the override data root — \
         the startup pin-scan must not import palaces from the real environment.\n\
         Override root: {}\n\
         Palaces found: {palace_dirs:?}",
        tmp.path().display()
    );

    // Safeguard: at most 1 palace directory should exist (the auto-seeded
    // default, if any). The real environment typically has several palaces;
    // if more than 1 appears here it strongly suggests the pin-scan leaked.
    assert!(
        palace_dirs.len() <= 1,
        "Isolated instance created {} palace directories — expected ≤ 1 (only the \
         auto-seeded default is allowed). Likely the pin-scan leaked real-env palaces.\n\
         Override root: {}\n\
         Palaces found: {palace_dirs:?}",
        palace_dirs.len(),
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
