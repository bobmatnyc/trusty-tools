//! Unit tests for the pm-guard presence rules (`pm_guard_presence.rs`), #8878 PR2a.
//!
//! Every spoof test asserts the verdict denies and that the grant files are
//! as they were: a refusal writes no `.presence`.

use std::path::PathBuf;

use serde_json::{Value, json};
use trusty_mpm::core::architect_launch::ARCHITECT_DIR;
use trusty_mpm::core::presence::{self, METHOD_LOCAL, grant_path};
use trusty_mpm::core::presence_key::GrantKey;
use trusty_mpm::core::twin_identity::ClaudeProcess;

use super::*;
use crate::commands::pm_guard_trust_anchor::evaluate;
use crate::commands::pm_guard_trust_anchor::tests::{
    ARCHITECT, Fixture, allowlist, architect_env, bound_env, fixture, grant_idle, key_lookup,
    payload, pm_env, test_key,
};

fn write_to(fx: &Fixture, path: &std::path::Path) -> Value {
    payload(
        fx,
        "Write",
        json!({ "file_path": path.display().to_string(), "content": "x" }),
    )
}

fn root(fx: &Fixture) -> PathBuf {
    fx.home.join(ANCHOR_ROOT)
}

/// Every `.presence` file with its bytes.
fn grants(fx: &Fixture) -> Vec<(PathBuf, Vec<u8>)> {
    let Ok(dir) = std::fs::read_dir(root(fx).join(ARCHITECT_DIR)) else {
        return Vec::new();
    };
    let mut out: Vec<_> = dir
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "presence"))
        .map(|p| {
            let bytes = std::fs::symlink_metadata(&p)
                .ok()
                .filter(|m| m.is_file())
                .and_then(|_| std::fs::read(&p).ok())
                .unwrap_or_default();
            (p, bytes)
        })
        .collect();
    out.sort();
    out
}

/// The bound Architect with the test key but the grant `prepare` leaves.
fn gated_env(fx: &Fixture) -> HookEnv {
    HookEnv {
        presence_key: key_lookup(),
        ..bound_env(fx)
    }
}

/// Assert the Architect's config write is denied with the presence remedy
/// and leaves every grant file as it was.
fn assert_presence_denied(fx: &Fixture, env: &HookEnv) -> String {
    let before = grants(fx);
    let reason = evaluate(&write_to(fx, &fx.anchor), env, || allowlist(fx))
        .expect("an Architect anchor write with no valid grant is denied");
    assert!(reason.contains("Presence required (#8878)"), "{reason}");
    assert!(reason.contains("`tm presence local`"), "{reason}");
    assert_eq!(grants(fx), before, "a refusal writes no .presence");
    reason
}

#[test]
fn an_architect_anchor_write_without_a_grant_is_denied_8878() {
    let fx = fixture();
    let reason = assert_presence_denied(&fx, &gated_env(&fx));
    assert!(reason.contains("no presence grant exists"), "{reason}");
    assert!(grants(&fx).is_empty());
    // No key source at all is a deny too, never an allow.
    grant_idle(&fx, 0);
    assert_presence_denied(&fx, &bound_env(&fx));
}

#[test]
fn an_architect_anchor_write_with_a_grant_passes_and_refreshes_8878() {
    let fx = fixture();
    let env = gated_env(&fx);
    grant_idle(&fx, 100 * 60);
    assert_eq!(
        evaluate(&write_to(&fx, &fx.anchor), &env, || allowlist(&fx)),
        None
    );
    let now = presence::unix_secs(std::time::SystemTime::now());
    let grant = presence::check(&root(&fx), &ARCHITECT, || Some(test_key()), now).expect("valid");
    assert!(
        now - grant.last_used < 60,
        "the guarded call refreshed the grant"
    );
    // A write that touches no anchor needs no grant.
    let plain = bound_env(&fx);
    std::fs::remove_file(grant_path(&root(&fx), ARCHITECT.pid)).expect("rm grant");
    assert_eq!(
        evaluate(&write_to(&fx, &fx.cwd.join("a.txt")), &plain, || allowlist(
            &fx
        )),
        None
    );
}

/// Spoof: D5b Q2 — a grant idle 121 minutes no longer lets the write through.
#[test]
fn a_grant_idle_121_minutes_denies_the_write_8878() {
    let fx = fixture();
    grant_idle(&fx, 121 * 60);
    let reason = assert_presence_denied(&fx, &gated_env(&fx));
    assert!(reason.contains("120 idle minutes"), "{reason}");
}

/// Spoof (Bob D5b Q3): a grant tm did not seal is refused.
#[test]
fn a_forged_grant_denies_the_write_8878() {
    let fx = fixture();
    let now = presence::unix_secs(std::time::SystemTime::now());
    let forger = GrantKey::from_bytes([0xee; 32]);
    presence::mint(&root(&fx), &ARCHITECT, METHOD_LOCAL, &forger, now).expect("mint");
    let reason = assert_presence_denied(&fx, &gated_env(&fx));
    assert!(reason.contains("not minted by tm"), "{reason}");
}

/// Spoof: a valid grant for another `claude`, copied to this one's file.
#[test]
fn a_grant_copied_from_another_pid_denies_the_write_8878() {
    let fx = fixture();
    let other = ClaudeProcess {
        pid: 61,
        start_time: 610,
    };
    let now = presence::unix_secs(std::time::SystemTime::now());
    presence::mint(&root(&fx), &other, METHOD_LOCAL, &test_key(), now).expect("mint");
    std::fs::copy(
        grant_path(&root(&fx), 61),
        grant_path(&root(&fx), ARCHITECT.pid),
    )
    .expect("copy");
    let reason = assert_presence_denied(&fx, &gated_env(&fx));
    assert!(reason.contains("another process"), "{reason}");
}

/// Spoof: the same PID with another start time (PID reuse).
#[test]
fn a_grant_with_a_stale_start_time_denies_the_write_8878() {
    let fx = fixture();
    let reused = ClaudeProcess {
        start_time: 599,
        ..ARCHITECT
    };
    let now = presence::unix_secs(std::time::SystemTime::now());
    presence::mint(&root(&fx), &reused, METHOD_LOCAL, &test_key(), now).expect("mint");
    let reason = assert_presence_denied(&fx, &gated_env(&fx));
    assert!(reason.contains("PID reuse"), "{reason}");
}

/// Spoof: a symlink at the grant path is not followed.
#[test]
fn a_symlinked_grant_denies_the_write_8878() {
    let fx = fixture();
    let path = grant_idle(&fx, 0);
    let real = fx.cwd.join("real.presence");
    std::fs::rename(&path, &real).expect("move");
    std::os::unix::fs::symlink(&real, &path).expect("symlink");
    let reason = assert_presence_denied(&fx, &gated_env(&fx));
    assert!(reason.contains("symlink"), "{reason}");
}

/// Spoof: a PM (or the Architect with no grant) writing a `.presence` is an
/// anchor write.
#[test]
fn a_pm_writing_a_presence_file_is_denied_8878() {
    let fx = fixture();
    let target = grant_path(&root(&fx), ARCHITECT.pid);
    let write = write_to(&fx, &target);
    let reason = evaluate(&write, &pm_env(&fx), || allowlist(&fx)).expect("a PM is denied");
    assert!(
        reason.contains("Trust-anchor write denied (#8878)"),
        "{reason}"
    );
    let bash = payload(
        &fx,
        "Bash",
        json!({ "command": format!("printf '{{}}' > {}", target.display()) }),
    );
    assert!(evaluate(&bash, &pm_env(&fx), || allowlist(&fx)).is_some());
    // The Architect needs a grant to write one, so it cannot mint its own.
    let reason = evaluate(&write, &gated_env(&fx), || allowlist(&fx)).expect("no grant");
    assert!(reason.contains("Presence required"), "{reason}");
    assert!(grants(&fx).is_empty(), "no .presence was written");
    // A subagent of the granted Architect is still an agent.
    let env = architect_env(&fx);
    let mut sub = write.clone();
    sub["agent_id"] = json!("agent-7");
    assert!(evaluate(&sub, &env, || allowlist(&fx)).is_some());
}

/// Bob Q9b: Remote Control is noted in the deny, and grants nothing.
#[test]
fn the_deny_notes_remote_control_without_granting_8878() {
    let fx = fixture();
    let env = HookEnv {
        bridge: true,
        ..gated_env(&fx)
    };
    let reason = assert_presence_denied(&fx, &env);
    assert!(reason.contains("Remote Control appears active"), "{reason}");
    let mut call = write_to(&fx, &fx.anchor);
    call["bridge_status"] = json!("connected");
    let reason = evaluate(&call, &gated_env(&fx), || allowlist(&fx)).expect("still denied");
    assert!(reason.contains("Remote Control appears active"), "{reason}");
    assert_eq!(rc_hint(&write_to(&fx, &fx.anchor), &gated_env(&fx)), "");
}

#[test]
fn every_spelling_of_a_presence_verb_is_found_8878() {
    for command in [
        "tm presence local",
        "TM presence local",
        "/usr/local/bin/tm presence local",
        "trusty-mpm presence local",
        "tm --url http://127.0.0.1:1 presence local",
        "tm fleet bind",
        "cd /tmp && tm fleet bind",
        "sh -c 'tm presence local'",
        "echo $(tm presence local)",
        "cargo run --bin tm -- presence local",
        "tm presence",
        "tm $VERB local",
    ] {
        assert!(evaluate_presence_verbs(command).is_some(), "{command}");
    }
    for command in [
        "tm fleet status",
        "tm fleet relaunch",
        "echo tm presence local",
        "grep -r 'tm presence local' docs",
        "tm session list",
    ] {
        assert_eq!(evaluate_presence_verbs(command), None, "{command}");
    }
}

/// Spoof (#8878 PR2b-1b): the new verbs are a PM's or an agent's to run no
/// more than `local` is.
#[test]
fn a_pm_calling_presence_request_or_devices_add_is_denied_by_the_floor_8878() {
    for command in [
        "tm presence request",
        "tm presence wait --timeout 5m",
        "tm presence devices add phone",
        "tm presence devices revoke abc",
        "tm presence devices list",
    ] {
        assert!(evaluate_presence_verbs(command).is_some(), "{command}");
    }
}

/// Spoof (#8878 PR2b-1b): a PM writing a device record or a pending request
/// is an anchor write, so it cannot enrol a device or forge a request.
#[test]
fn a_pm_writing_devices_or_pending_is_denied_8878() {
    let fx = fixture();
    let anchor = root(&fx).join(ARCHITECT_DIR);
    for target in [
        anchor.join("devices").join("cred.json"),
        anchor
            .join("pending")
            .join(format!("{}.json", "0".repeat(32))),
        anchor.join("devices").join(".ticket-00"),
    ] {
        let reason = evaluate(&write_to(&fx, &target), &pm_env(&fx), || allowlist(&fx))
            .expect("a PM is denied");
        assert!(
            reason.contains("Trust-anchor write denied (#8878)"),
            "{reason}"
        );
    }
    assert!(grants(&fx).is_empty());
}

/// Fail-open 11 (#8878 PR2b-1b): a request that fails (daemon down, timeout,
/// rate limit) leaves the deny text exactly as with no request; a request
/// that lands names its id and `tm presence wait`. Neither writes a grant.
#[test]
fn hook_request_failure_does_not_change_the_deny_8878() {
    let fx = fixture();
    let plain = assert_presence_denied(&fx, &gated_env(&fx));
    let failing = HookEnv {
        presence_key: key_lookup().with_request(|_| None),
        ..bound_env(&fx)
    };
    assert_eq!(assert_presence_denied(&fx, &failing), plain);
    let asked = HookEnv {
        presence_key: key_lookup().with_request(|_| Some("abc123".to_owned())),
        ..bound_env(&fx)
    };
    let reason = assert_presence_denied(&fx, &asked);
    assert!(reason.starts_with(&plain[..plain.len() / 2]), "{reason}");
    assert!(
        reason.contains("tm presence wait --request abc123"),
        "{reason}"
    );
    assert!(reason.contains("unverified origin"), "{reason}");
    assert!(grants(&fx).is_empty());
}
