//! `service restart` terminates a detached daemon and verifies the result
//! (#8686).
//!
//! Why: `launchctl bootout` left a daemon detached from launchd (PPID 1)
//! serving the old version on :7878, and the restart read as successful.
//! What: drives [`restart_with`] over recorded effects. PID 1685 stands for the
//! detached daemon from the report: bootout does not stop it.
//! Test: this module.

use std::cell::RefCell;

use super::service_restart::{restart_with, version_from_health};

const DETACHED: u32 = 1685;

/// #8686: the detached PID that survives bootout is terminated, and only then
/// is the unit bootstrapped.
#[test]
fn a_detached_daemon_is_terminated_before_the_bootstrap() {
    let steps = RefCell::new(Vec::<String>::new());
    let dead = RefCell::new(Vec::<u32>::new());
    let report = restart_with(
        vec![DETACHED],
        "0.55.0",
        || {
            steps.borrow_mut().push("bootout".into());
            Ok(())
        },
        |pid| !dead.borrow().contains(&pid),
        |pids| {
            steps.borrow_mut().push(format!("terminate {pids:?}"));
            dead.borrow_mut().extend_from_slice(pids);
            Vec::new()
        },
        || {
            steps.borrow_mut().push("bootstrap".into());
            Ok(())
        },
        || Ok("0.55.0".to_string()),
    )
    .expect("the restart succeeds");
    assert_eq!(
        steps.into_inner(),
        vec!["bootout", "terminate [1685]", "bootstrap"]
    );
    assert_eq!(report.replaced, vec![DETACHED]);
    assert_eq!(report.version, "0.55.0");
}

/// #8686: a PID that outlives SIGTERM and SIGKILL stops the restart before the
/// bootstrap, which would otherwise crash-loop against the held port.
#[test]
fn a_survivor_aborts_before_the_bootstrap() {
    let bootstrapped = RefCell::new(false);
    let err = restart_with(
        vec![DETACHED],
        "0.55.0",
        || Ok(()),
        |_| true,
        |pids| pids.to_vec(),
        || {
            *bootstrapped.borrow_mut() = true;
            Ok(())
        },
        || Ok("0.55.0".to_string()),
    )
    .expect_err("a surviving old daemon fails the restart");
    assert!(!bootstrapped.into_inner(), "no bootstrap onto a held port");
    assert!(err.to_string().contains("1685"), "{err}");
}

/// #8686 acceptance: `/health` must report the new version; the old version
/// answering means an old daemon is still serving.
#[test]
fn an_old_version_on_health_fails_the_restart() {
    let err = restart_with(
        vec![DETACHED],
        "0.54.3",
        || Ok(()),
        |_| false,
        |_| Vec::new(),
        || Ok(()),
        || Ok("0.54.2".to_string()),
    )
    .expect_err("an old version on /health fails the restart");
    assert!(err.to_string().contains("0.54.2"), "{err}");
}

/// The health probe reads `version` from the report.
#[test]
fn health_version_is_read_from_the_report() {
    let report = serde_json::json!({"status": "ok", "version": "0.55.0", "indexes": 3});
    assert_eq!(version_from_health(&report).as_deref(), Some("0.55.0"));
    assert_eq!(version_from_health(&serde_json::json!({})), None);
}
