//! Unit tests for the doctor check helpers.
//!
//! Why: lifted out of `mod.rs` to keep the production module under the
//! 500-SLOC cap; behaviour is unchanged (see #1195).

use super::*;
use serial_test::serial;

#[test]
fn check_result_classifiers() {
    let ok = CheckResult::Ok("good".into());
    let warn = CheckResult::Warn("maybe".into());
    let err = CheckResult::Error("bad".into());
    assert!(!ok.is_error() && !ok.is_warn());
    assert!(warn.is_warn() && !warn.is_error());
    assert!(err.is_error() && !err.is_warn());
}

#[test]
fn check_daemon_running_ok_branch() {
    let r = check_daemon_running(true, "http://127.0.0.1:7878", "0.3.27");
    match r {
        CheckResult::Ok(msg) => {
            assert!(msg.contains("127.0.0.1:7878"));
            assert!(msg.contains("0.3.27"));
        }
        other => panic!("expected Ok, got {:?}", other),
    }
}

#[test]
fn check_daemon_running_error_branch() {
    let r = check_daemon_running(false, "http://127.0.0.1:7878", "");
    assert!(r.is_error());
    match r {
        CheckResult::Error(msg) => assert!(msg.contains("trusty-search start")),
        _ => panic!("expected Error variant"),
    }
}

#[test]
fn summarize_indexes_all_populated_singular() {
    let r = summarize_indexes(1, 0);
    match r {
        CheckResult::Ok(msg) => {
            assert!(msg.contains("1 index registered"));
            assert!(!msg.contains("indexes"));
        }
        other => panic!("expected Ok, got {:?}", other),
    }
}

#[test]
fn summarize_indexes_all_populated_plural() {
    let r = summarize_indexes(5, 0);
    match r {
        CheckResult::Ok(msg) => {
            assert!(msg.contains("5 indexes"));
            assert!(msg.contains("all have chunks"));
        }
        _ => panic!("expected Ok"),
    }
}

#[test]
fn summarize_indexes_some_empty_singular() {
    let r = summarize_indexes(3, 1);
    match r {
        CheckResult::Warn(msg) => {
            assert!(msg.contains("3 indexes"));
            assert!(msg.contains("1 has no chunks"));
        }
        _ => panic!("expected Warn"),
    }
}

#[test]
fn summarize_indexes_some_empty_plural() {
    let r = summarize_indexes(4, 2);
    match r {
        CheckResult::Warn(msg) => {
            assert!(msg.contains("4 indexes"));
            assert!(msg.contains("2 have no chunks"));
        }
        _ => panic!("expected Warn"),
    }
}

#[test]
fn check_data_dir_missing_warns() {
    let path = std::path::Path::new("/nonexistent/trusty-search-doctor-test-zzz");
    let r = check_data_dir(path);
    match r {
        CheckResult::Warn(msg) => assert!(msg.contains("does not exist")),
        other => panic!("expected Warn for missing dir, got {:?}", other),
    }
}

#[test]
fn check_data_dir_writable_ok() {
    let tmp = std::env::temp_dir().join(format!(
        "trusty-search-doctor-writable-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let r = check_data_dir(&tmp);
    assert!(
        !r.is_error(),
        "writable tempdir should not be Error: {:?}",
        r
    );
    match r {
        CheckResult::Ok(msg) => assert!(msg.contains("writable")),
        _ => panic!("expected Ok for writable tempdir"),
    }
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn check_lock_file_absent_is_ok() {
    let tmp = std::env::temp_dir().join(format!(
        "trusty-search-doctor-lock-absent-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let r = check_lock_file(&tmp, false);
    match r {
        CheckResult::Ok(msg) => assert!(msg.contains("healthy")),
        other => panic!("expected Ok when no lock file, got {:?}", other),
    }
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn check_lock_file_invalid_pid_warns() {
    let tmp = std::env::temp_dir().join(format!(
        "trusty-search-doctor-lock-invalid-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    std::fs::write(tmp.join("daemon.lock"), "not-a-pid").unwrap();
    let r = check_lock_file(&tmp, false);
    assert!(r.is_warn(), "garbage lock content should warn: {:?}", r);
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn check_lock_file_stale_pid_warns() {
    let tmp = std::env::temp_dir().join(format!(
        "trusty-search-doctor-lock-stale-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    // PID 0 is reserved on Unix and `kill(0, 0)` is the "any process in
    // our group" semantic — but on macOS kill(0) targets the group itself
    // and may succeed. Use a very high PID that's almost certainly unused.
    std::fs::write(tmp.join("daemon.lock"), "4194303").unwrap();
    let r = check_lock_file(&tmp, false);
    assert!(r.is_warn(), "stale PID should warn: {:?}", r);
    match r {
        CheckResult::Warn(msg) => {
            assert!(msg.contains("4194303") || msg.contains("Stale"));
        }
        _ => unreachable!(),
    }
    let _ = std::fs::remove_dir_all(&tmp);
}

#[tokio::test]
async fn check_port_reachable_unbound_port_errors() {
    // Port 65535 is unlikely to be bound; assert we get an Error variant.
    let r = check_port_reachable(65535).await;
    assert!(r.is_error(), "unbound port should be Error: {:?}", r);
}

#[test]
fn read_daemon_port_returns_some_u16() {
    // Smoke-test: returns a port (default or from file). Function should
    // never panic and should return a value in the valid port range.
    let p = read_daemon_port();
    // Default port path can include 0 if the port file held garbage that
    // parses as 0, but normally it's > 0. Just assert it returns a u16
    // (trivially true) and doesn't panic.
    let _ = p;
}

/// #3673: `doctor_data_dir()` reads the process-global `TRUSTY_DATA_DIR` env
/// var, which 40+ other tests across this test binary also mutate (e.g.
/// `service/data_dir.rs`, `commands/start/tests.rs`). Without `#[serial]`
/// this test can observe a sibling test's tempdir value mid-flight and fail
/// an assertion that has nothing to do with this test's own behaviour.
///
/// Why: `#[serial]` (bare — the crate-wide default lock, matching the
/// convention in `service/data_dir.rs`'s `data_dir_override_yields_absolute_path`
/// and `commands/daemon_utils.rs`) serializes this test against every other
/// `TRUSTY_DATA_DIR`-mutating test in the crate. We additionally clear
/// `TRUSTY_DATA_DIR` ourselves (rather than merely hoping no sibling left it
/// set) so the assertion always exercises the platform-default fallback path
/// — the one that actually contains `"trusty-search"` — deterministically,
/// and restore whatever was there beforehand so we don't pollute later tests.
/// What: clear `TRUSTY_DATA_DIR` under the lock, call `doctor_data_dir()`,
/// assert the default fallback path contains `"trusty-search"`, restore.
/// Test: `doctor_data_dir_returns_non_empty_path`.
#[test]
#[serial]
fn doctor_data_dir_returns_non_empty_path() {
    let prev = std::env::var("TRUSTY_DATA_DIR").ok();
    unsafe { std::env::remove_var("TRUSTY_DATA_DIR") };
    let p = doctor_data_dir();
    match prev {
        Some(v) => unsafe { std::env::set_var("TRUSTY_DATA_DIR", v) },
        None => unsafe { std::env::remove_var("TRUSTY_DATA_DIR") },
    }
    assert!(p.to_string_lossy().contains("trusty-search"));
}

#[test]
fn fastembed_cache_dir_respects_env_override() {
    // Set a unique override value and assert the function returns exactly it.
    // SAFETY: process-global env vars are not test-isolated; this test
    // assumes it runs single-threaded for this variable's lifetime. We
    // save+restore to avoid polluting sibling tests.
    let key = "FASTEMBED_CACHE_DIR";
    let prev = std::env::var(key).ok();
    // Tests in a single binary may run in parallel; this is an accepted
    // test-only flakiness risk for env-var manipulation.
    std::env::set_var(key, "/tmp/trusty-search-fastembed-test-override");
    let got = fastembed_cache_dir();
    assert_eq!(
        got,
        std::path::PathBuf::from("/tmp/trusty-search-fastembed-test-override")
    );
    // Restore previous value.
    match prev {
        Some(v) => std::env::set_var(key, v),
        None => std::env::remove_var(key),
    }
}

// ── Python/MPS embedder checks (epic #3524 slice 5) ─────────────────────────

/// Save/restore a single env var across a test body — same accepted
/// env-manipulation-flakiness trade-off as `fastembed_cache_dir_respects_env_override`
/// above.
struct EnvVarGuard {
    key: &'static str,
    prev: Option<String>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let prev = std::env::var(key).ok();
        std::env::set_var(key, value);
        Self { key, prev }
    }

    fn remove(key: &'static str) -> Self {
        let prev = std::env::var(key).ok();
        std::env::remove_var(key);
        Self { key, prev }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match &self.prev {
            Some(v) => std::env::set_var(self.key, v),
            None => std::env::remove_var(self.key),
        }
    }
}

/// Build a `VenvLayout` rooted at a fresh temp dir — none of its paths exist
/// on disk until a test creates them, matching a never-bootstrapped venv.
fn fake_layout(base: std::path::PathBuf) -> trusty_embedderd_py::VenvLayout {
    let project_dir = base.join("project");
    let venv_dir = base.join("venv");
    let venv_python = venv_dir.join("bin").join("python");
    trusty_embedderd_py::VenvLayout {
        base,
        project_dir,
        venv_dir,
        venv_python,
    }
}

// `#[serial]` (bare — the crate-wide default lock, matching the convention
// established elsewhere in this crate, e.g. `start/tests.rs`'s
// `missing_binary_fails_fast_with_install_hint`): these three tests mutate
// the process-global `TRUSTY_EMBEDDER` env var, which `doctor_pipeline.rs`'s
// `PythonEmbedderCheck` tests also mutate — without `#[serial]` here, `cargo
// test`'s default parallel threads race on the same env var across the two
// files and can flip a "disabled" assertion into an "enabled" one (or vice
// versa) depending on scheduling (caught by CI, PR #3560).

#[test]
#[serial]
fn python_embedder_enabled_true_only_for_python_value() {
    let _g = EnvVarGuard::set("TRUSTY_EMBEDDER", "python");
    assert!(python_embedder_enabled());
}

#[test]
#[serial]
fn python_embedder_enabled_false_when_unset() {
    let _g = EnvVarGuard::remove("TRUSTY_EMBEDDER");
    assert!(!python_embedder_enabled());
}

#[test]
#[serial]
fn python_embedder_enabled_false_for_other_values() {
    let _g = EnvVarGuard::set("TRUSTY_EMBEDDER", "stdio");
    assert!(!python_embedder_enabled());
}

#[test]
fn check_python_venv_missing_reports_not_yet_bootstrapped() {
    // Why: a brand-new install (no `.ready`, no venv python) must produce a
    // "not yet bootstrapped" Warn — not an Error, since this is expected on
    // first opt-in — and must never touch the filesystem beyond reading.
    let tmp = tempfile::tempdir().expect("tempdir");
    let layout = fake_layout(tmp.path().join("py-embedder").join("deadbeef"));

    let r = check_python_venv(&layout);
    assert!(r.is_warn(), "expected Warn, got {r:?}");
    match r {
        CheckResult::Warn(msg) => assert!(
            msg.contains("not yet bootstrapped"),
            "unexpected message: {msg}"
        ),
        other => panic!("expected Warn, got {other:?}"),
    }
}

#[test]
fn check_python_venv_ready_and_current_reports_ok() {
    // Why: the happy path — venv python present AND `.ready` matches the
    // CURRENT lockfile hash — must report Ok, not a false Warn.
    let tmp = tempfile::tempdir().expect("tempdir");
    let layout = fake_layout(tmp.path().join("py-embedder").join("deadbeef"));
    std::fs::create_dir_all(layout.venv_python.parent().unwrap()).unwrap();
    std::fs::write(&layout.venv_python, b"#!/bin/sh\n").unwrap();
    std::fs::create_dir_all(&layout.base).unwrap();
    let current_hash = trusty_embedderd_py::bootstrap::lockfile_hash();
    std::fs::write(layout.base.join(".ready"), &current_hash).unwrap();

    let r = check_python_venv(&layout);
    assert!(!r.is_error(), "expected Ok/Warn, got Error: {r:?}");
    match r {
        CheckResult::Ok(msg) => assert!(msg.contains("ready"), "unexpected message: {msg}"),
        other => panic!("expected Ok, got {other:?}"),
    }
}

#[test]
fn check_python_venv_stale_hash_reports_warn() {
    // Why: a `.ready` sentinel that does NOT match the current lockfile hash
    // (e.g. after a `uv.lock` update shipped in a new trusty-search release)
    // must be treated as stale, not silently trusted.
    let tmp = tempfile::tempdir().expect("tempdir");
    let layout = fake_layout(tmp.path().join("py-embedder").join("deadbeef"));
    std::fs::create_dir_all(layout.venv_python.parent().unwrap()).unwrap();
    std::fs::write(&layout.venv_python, b"#!/bin/sh\n").unwrap();
    std::fs::create_dir_all(&layout.base).unwrap();
    std::fs::write(layout.base.join(".ready"), "0000000000000000-stale").unwrap();

    let r = check_python_venv(&layout);
    assert!(r.is_warn(), "expected Warn, got {r:?}");
    match r {
        CheckResult::Warn(msg) => assert!(
            msg.contains("stale or corrupt"),
            "unexpected message: {msg}"
        ),
        other => panic!("expected Warn, got {other:?}"),
    }
}

#[test]
fn check_python_venv_ready_file_without_venv_binary_reports_warn() {
    // Why: a half-deleted venv (`.ready` present, but the venv python binary
    // itself is gone) must not be trusted as "ready".
    let tmp = tempfile::tempdir().expect("tempdir");
    let layout = fake_layout(tmp.path().join("py-embedder").join("deadbeef"));
    std::fs::create_dir_all(&layout.base).unwrap();
    let current_hash = trusty_embedderd_py::bootstrap::lockfile_hash();
    std::fs::write(layout.base.join(".ready"), &current_hash).unwrap();
    // Deliberately do NOT create layout.venv_python.

    let r = check_python_venv(&layout);
    assert!(r.is_warn(), "expected Warn, got {r:?}");
}
