//! End-to-end proof of the #9791 deny: `file -f`/`-m` on a secret-bearing file.
//!
//! Why: `file -f .env` echoes every line of `.env` in its error output, and
//! the guard allowed it because `file` is a safe-handling verb by name. Only
//! the built binary proves the rule sits on both the guarded path and the
//! bypass floor, as the #8625 kuzu-store rule does.
//! What: the #8878 harness — `tm hook --pm-guard` with a scratch `$HOME`, run
//! as a PM, as a subagent, under `TRUSTY_MPM_PM_UNRESTRICTED` and
//! `TRUSTY_MPM_DISABLE_HOOKS`, and as the recorded Architect's main thread.
//! The project holds a `.env` with an obviously fake value; no output may
//! carry it.
//! Test: `cargo test -p trusty-mpm --test integration tm_hook_pm_guard_secret_file_list_9791::`.

use crate::tm_hook_pm_guard_trust_anchor_8878::{
    Fixture, bash_payload, run, run_under_claude, run_under_claude_with,
};

/// The bypass variables, and `None` for no bypass.
const BYPASSES: [Option<(&str, &str)>; 3] = [
    None,
    Some(("TRUSTY_MPM_PM_UNRESTRICTED", "1")),
    Some(("TRUSTY_MPM_DISABLE_HOOKS", "1")),
];

/// The fake value the fixture's `.env` holds; it must never reach the output.
const FAKE_VALUE: &str = "obviously-fake-value-9791";

/// The calls the rule must refuse.
const DENIED: &[&str] = &[
    "file -f .env",
    "file --files-from .env",
    "file --magic-file=.env",
    "bash -c \"file -m .env\"",
    "sudo /usr/bin/file -bf .env 2>&1 | head",
];

/// The #8878 fixture plus a `.env` in the project.
fn fixture() -> Fixture {
    let fx = Fixture::new();
    std::fs::write(
        fx.project.join(".env"),
        format!("FAKE_ENTRY={FAKE_VALUE}\n"),
    )
    .expect("env");
    fx
}

/// `payload` as a dispatched subagent's call.
fn as_subagent(payload: &str) -> String {
    let mut value: serde_json::Value = serde_json::from_str(payload).expect("json");
    value["agent_id"] = serde_json::json!("agent-7");
    value.to_string()
}

/// Whether `out` is the #9791 deny, labelled and free of the fake value.
fn is_the_deny(out: &str) -> bool {
    out.contains("\"deny\"")
        && out.contains("secret-file-read")
        && out.contains("#9791")
        && !out.contains(FAKE_VALUE)
}

/// 🔴 REGRESSION (#9791): allowed on 59fff9bc7e for every caller.
#[test]
fn the_file_list_deny_holds_under_each_bypass_and_for_a_subagent() {
    let fx = fixture();
    for command in DENIED {
        let payload = bash_payload(&fx, command);
        for bypass in BYPASSES {
            let env: Vec<(&str, &str)> = bypass.into_iter().collect();
            for stdin in [payload.clone(), as_subagent(&payload)] {
                let out = run(&fx, &stdin, &env, None);
                assert!(is_the_deny(&out), "{bypass:?} {stdin}: {out}");
            }
        }
    }
}

/// 🔴 REGRESSION (#9791): the Architect's env-file exemption does not lift it.
#[test]
fn the_architect_is_bound_by_the_file_list_deny() {
    let fx = fixture();
    for command in DENIED {
        let payload = bash_payload(&fx, command);
        let out = run_under_claude(&fx, &payload, true);
        assert!(is_the_deny(&out), "{command}: {out}");
        for bypass in BYPASSES.into_iter().flatten() {
            let out = run_under_claude_with(&fx, &payload, true, &[bypass]);
            assert!(is_the_deny(&out), "{bypass:?} {command}: {out}");
        }
    }
}

/// #9791 controls: plain `file .env` and a quoted `--body` naming the call.
#[test]
fn plain_file_and_a_quoted_body_are_not_denied_by_the_rule() {
    let fx = fixture();
    for command in [
        "file .env",
        "gh issue comment 9791 --body \"file -f .env prints the lines\"",
    ] {
        let payload = as_subagent(&bash_payload(&fx, command));
        let out = run(&fx, &payload, &[], None);
        assert!(!out.contains("\"deny\""), "{command}: {out}");
        assert!(!out.contains(FAKE_VALUE), "{command}: {out}");
    }
}
