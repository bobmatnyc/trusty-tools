//! Tests for `upgrade.rs`.
//!
//! Why a sibling file: `upgrade.rs` is production source under the 500-SLOC cap
//! and #4964 pushed it over. Moving the test bodies to a `_tests.rs` sibling
//! (3000-SLOC test cap) is the split `service_bootstrap.rs` /
//! `service_bootstrap_tests.rs` already uses here.
//!
//! What: the report shaping and exit codes, the TTY/consent invariant, and the
//! #4964 Phase 1 restart-plan and no-`upgrade_and_restart` guards.

use super::*;

fn candidate() -> UpdateCandidate {
    UpdateCandidate {
        crate_name: "trusty-search".to_owned(),
        binary: "trusty-search".to_owned(),
        installed: Some("0.19.0".to_owned()),
        latest: "0.20.0".to_owned(),
        daemon: true,
        is_install: false,
    }
}

/// Build an `UpgradeOutcome` with the pre-#3554 default shadow state
/// (clear — `shadow_ok: true`, empty detail), so tests that only care
/// about the binary-upgrade dimension don't have to repeat those fields.
fn outcome(member: &str, ok: bool, detail: &str) -> UpgradeOutcome {
    UpgradeOutcome {
        member: member.to_owned(),
        ok,
        detail: detail.to_owned(),
        shadow_ok: true,
        shadow_detail: String::new(),
    }
}

/// Why: The non-applying envelope is a public contract; pin its shape.
/// What: Builds a needs-confirmation report; asserts keys.
/// Test: This is the test.
#[test]
fn report_serialises() {
    let report = UpgradeReport::non_applying("needs_confirmation", vec![candidate()]);
    let v = serde_json::to_value(&report).expect("serialises");
    assert_eq!(v["command"], "upgrade");
    assert_eq!(v["status"], "needs_confirmation");
    assert_eq!(v["candidates"][0]["crate_name"], "trusty-search");
    assert_eq!(v["members"].as_array().expect("array").len(), 0);
}

/// Why: An applied report's `all_ok` must reflect member outcomes.
/// What: Mixes ok + failed; asserts all_ok false and status "applied".
/// Test: This is the test.
#[test]
fn applied_report_all_ok() {
    let report = UpgradeReport::applied(
        vec![candidate()],
        vec![outcome("a", true, ""), outcome("b", false, "boom")],
    );
    assert!(!report.all_ok);
    assert_eq!(report.status, "applied");
}

/// Why: #3554 review (HIGH) — a just-upgraded binary that is provably
/// PATH-shadowed by a stale copy is a genuine "looks upgraded, actually
/// not live" failure; the report must not claim `all_ok: true` in that
/// case, mirroring `install::tests::report_all_ok_reflects_shadow_failure`.
/// What: one member with `ok: true, shadow_ok: false` → `all_ok == false`
/// and exit code 2; a `shadow_ok: true` (clear) member → `all_ok == true`.
/// Test: this is the test.
#[test]
fn applied_report_all_ok_reflects_shadow_failure() {
    let shadowed = UpgradeReport::applied(
        vec![candidate()],
        vec![UpgradeOutcome {
            member: "trusty-mpm".to_owned(),
            ok: true,
            detail: "upgraded to 0.19.29".to_owned(),
            shadow_ok: false,
            shadow_detail: "PATH SHADOWED: installed trusty-mpm 0.19.29 to \
                            /x/.local/bin/tm, but the shell resolves `tm` to \
                            /x/.cargo/bin/tm (0.19.26)"
                .to_owned(),
        }],
    );
    assert!(
        !shadowed.all_ok,
        "a genuine PATH-shadow condition must flip all_ok to false"
    );
    assert_eq!(shadowed.exit_code(), 2);

    let clear = UpgradeReport::applied(
        vec![candidate()],
        vec![outcome("trusty-mpm", true, "upgraded to 0.19.29")],
    );
    assert!(
        clear.all_ok,
        "a clear (non-shadowed) upgrade must not flip all_ok"
    );
    assert_eq!(clear.exit_code(), 0);
}

/// Why: Exit codes are the automation contract; pin each status mapping.
/// What: Asserts the four status → exit-code mappings.
/// Test: This is the test.
#[test]
fn exit_codes_per_status() {
    assert_eq!(
        UpgradeReport::non_applying("nothing_to_do", vec![]).exit_code(),
        0
    );
    assert_eq!(
        UpgradeReport::non_applying("needs_confirmation", vec![candidate()]).exit_code(),
        3
    );
    assert_eq!(
        UpgradeReport::non_applying("declined", vec![candidate()]).exit_code(),
        4
    );
    let applied_ok = UpgradeReport::applied(vec![candidate()], vec![outcome("a", true, "")]);
    assert_eq!(applied_ok.exit_code(), 0);
}

/// Why: The load-bearing safety invariant from re-review: the `tty` value
/// that gates the interactive prompt MUST be the same value fed to
/// `decide_apply` as `is_tty`. A divergence would let a user answer "yes"
/// while the gate silently treats the session as non-interactive (or the
/// reverse). This pins the single-source-of-truth `tty` binding.
/// What: For every (has_candidates, yes, tty, json) combination, asserts
/// (a) `build_apply_inputs.is_tty` equals the probed `tty`, and (b) whenever
/// `should_prompt` is true the same `tty` was true — so the consent path and
/// the gate's TTY view are always consistent.
/// Test: This is the test.
#[test]
fn prompt_gate_matches_apply_inputs() {
    for &has_candidates in &[true, false] {
        for &yes in &[true, false] {
            for &tty in &[true, false] {
                for &json in &[true, false] {
                    let prompt = should_prompt(has_candidates, yes, tty, json);
                    // The consent the prompt would yield is irrelevant to
                    // the invariant; what matters is the TTY value is shared.
                    let inputs = build_apply_inputs(has_candidates, yes, tty, prompt);
                    // (a) the gate sees exactly the probed tty.
                    assert_eq!(inputs.is_tty, tty);
                    // (b) we only ever prompt when that same tty is true.
                    if prompt {
                        assert!(tty, "prompted without a TTY the gate also sees");
                    }
                }
            }
        }
    }
}

/// Why: An unknown member must fail fast at selection (exit 3) and never
/// reach the network/apply path — a pure, offline-safe guard test.
/// What: Calls `run` with a bogus member in `--json` mode; asserts exit 3.
/// Test: This is the test.
#[test]
fn run_unknown_member_is_error() {
    let code = run(false, false, false, true, &["not-a-tool".to_owned()], true);
    assert_eq!(code, 3);
}

/// Why: `--check` must be read-only (delegates to `updates`) and never reach
/// the apply path. This performs a live crates.io probe, so it is
/// `#[ignore]`-tagged to keep CI fast and offline-deterministic.
/// What: Calls `run` with `check = true`; asserts exit 0 (read-only).
/// Test: `cargo test -p trusty-installer -- --include-ignored`.
#[test]
#[ignore = "performs a live crates.io probe; run with --include-ignored"]
fn run_check_is_readonly() {
    let code = run(true, false, false, false, &["tga".to_owned()], true);
    assert_eq!(code, 0);
}

// ── #4964 Phase 1: the restart happens, and it is the only thing that
//    happens on the prebuilt branch ────────────────────────────────────
//
// These do NOT invoke the restart — `restart_member` bounces real daemons.
// They pin the decision, which is where the defect lived: every daemon
// member's "restart" resolved to `std::process::exit(1)` in a process
// launchd does not supervise, so nothing was ever bounced.

/// Why: the load-bearing Phase 1a assertion. Every daemon member must
/// PLAN a restart. Before the fix all of them planned one in name only.
/// What: on macOS, the five launchd daemons plan `Launchd` and trusty-mpm
/// plans `OwnVerb` — matching what `tctl restart` does for the same
/// members, which is the point of sharing the implementation.
/// Test: this is the test.
#[test]
fn restart_plan_daemons_restart() {
    use super::super::stable_set::ManageStrategy;
    for binary in [
        "trusty-search",
        "trusty-memory",
        "trusty-analyze",
        "trusty-console",
    ] {
        assert_eq!(
            restart_plan(binary, true, true),
            RestartPlan::Restart(ManageStrategy::Launchd),
            "{binary} must be bounced through launchd after an upgrade"
        );
    }
    // #6290: upgrading trusty-review has nothing to bounce — the next `run`
    // invocation IS the new binary. Planning a launchd restart would target
    // `com.trusty.review`, the unit this release evicts.
    assert_eq!(
        restart_plan("trusty-review", true, true),
        RestartPlan::NoRestart("no restart needed (not a daemon)".to_owned()),
        "a per-invocation member has no process to restart"
    );
    assert_eq!(
        restart_plan("trusty-mpm", true, true),
        RestartPlan::Restart(ManageStrategy::OwnVerb),
        "trusty-mpm is process-managed, not launchd"
    );
    // Process-managed members restart on any platform.
    assert_eq!(
        restart_plan("trusty-mpm", true, false),
        RestartPlan::Restart(ManageStrategy::OwnVerb)
    );
}

/// Why: `tga` is the one non-daemon in the stable set; bouncing it would be
/// meaningless and `restart_member` would try to `launchctl bootout` a
/// label that does not exist.
/// What: a non-daemon plans no restart.
/// Test: this is the test.
#[test]
fn restart_plan_non_daemon_is_a_noop() {
    assert!(matches!(
        restart_plan("tga", false, true),
        RestartPlan::NoRestart(_)
    ));
}

/// Why: launchd is macOS-only. A Linux host has no supervisor to bounce, so
/// the upgrade must report the binary landed and say what is left to do —
/// not fail because `launchd_control` bails on the platform.
/// What: a launchd member off macOS plans no restart, with a note naming
/// the binary.
/// Test: this is the test.
#[test]
fn restart_plan_launchd_member_off_macos_is_manual() {
    match restart_plan("trusty-search", true, false) {
        RestartPlan::NoRestart(note) => {
            assert!(note.contains("trusty-search"), "{note}");
            assert!(note.contains("manually"), "{note}");
        }
        other => panic!("expected a manual-restart note off macOS, got {other:?}"),
    }
}

/// Why (#4964 Phase 1b): the double-write. `upgrade_and_restart` runs
/// `cargo install <crate> --locked` unconditionally, so calling it from the
/// prebuilt branch — where the binary is already on disk — wrote the same
/// binary to two directories in one command, and errored out on a machine
/// with no Rust toolchain AFTER the new binary had landed. Six of the seven
/// stable-set members are daemons, so it fired on nearly every upgrade.
///
/// Asserting the outcome of that call is not possible offline (it needs a
/// real registry and a real daemon), so this pins the mechanism instead: no
/// call site in this crate. It goes red the moment the call is
/// reintroduced anywhere under `src/`, which is the exact regression.
///
/// What: walks every `.rs` file under this crate's `src/` and asserts no
/// non-comment line contains the CALL form — the path-qualified
/// `…update::upgrade_and_restart(`. Matching the call form rather than the
/// bare identifier is what lets this file keep naming the symbol in prose
/// (and in this assertion) without matching itself; the cost is that a
/// re-export imported under a different name would slip past, which is not
/// the shape that went wrong here.
/// Test: this is the test.
#[test]
fn no_installer_call_site_invokes_upgrade_and_restart() {
    // Assembled at runtime so this line does not match its own needle.
    let call = format!("update::{}(", "upgrade_and_restart");
    let call = call.as_str();
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut offenders: Vec<String> = Vec::new();
    let mut stack = vec![src];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("src must be readable") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("source must be readable");
            for (n, line) in text.lines().enumerate() {
                let t = line.trim_start();
                if t.starts_with("//") || t.starts_with('*') {
                    continue;
                }
                if line.contains(call) {
                    offenders.push(format!("{}:{}", path.display(), n + 1));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "tctl must not call `upgrade_and_restart`: it runs `cargo install` even when \
         the prebuilt already landed (the #4964 double-write), and its restart is \
         `std::process::exit(1)`, which restarts nothing when the caller is a terminal \
         process launchd does not supervise. Restart via `lifecycle::restart_member` \
         instead.\nfound: {offenders:#?}"
    );
}
