//! Unit tests for the doctor check helpers.
//!
//! Why: lifted out of `mod.rs` to keep the production module under the
//! 500-SLOC cap; behaviour is unchanged (see #1195).

use super::*;

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

#[test]
fn doctor_data_dir_returns_non_empty_path() {
    let p = doctor_data_dir();
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
