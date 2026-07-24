//! Unit tests for the `verify_tail` module.
//!
//! Why: Keeping tests in a sibling file rather than inline in `verify_tail.rs`
//! lets the definition file stay under the 500-SLOC production cap
//! (CLAUDE.md / `scripts/check_line_cap.sh`) while retaining full coverage —
//! mirrors the `install.rs` / `install_tests.rs` split. `verify_tail.rs`
//! previously stayed under the cap by splitting `verify_launchd_state.rs`
//! out of it (#3836 code-critic MEDIUM fix); it grew past the cap again once
//! the #3849 code-critic MEDIUM 1 + MEDIUM 2 fixes (bounded post-fallback
//! re-poll + an injectable `ServiceEnv` for the `NotLoaded` second-layer
//! repair, plus their four new unit tests) landed, so tests move out here.
//!
//! Test: `cargo test -p trusty-installer` runs all tests in this file.

use super::*;

fn row(member: &str, health: &str) -> VerifyRow {
    VerifyRow {
        member: member.to_owned(),
        health: health.to_owned(),
        kickstarted: false,
        required: true,
        budget_exhausted: false,
        down_state: None,
        verify_bootstrapped: false,
    }
}

/// Like `row`, but marks the member OPTIONAL (graceful-degrade,
/// demo-critical fix).
fn optional_row(member: &str, health: &str) -> VerifyRow {
    VerifyRow {
        required: false,
        ..row(member, health)
    }
}

/// Why: only a `down` LAUNCHD daemon is the #2498 failure signature.
/// What: asserts the truth table across health x manage-strategy.
/// Test: This is the test.
#[test]
fn needs_kickstart_only_for_down_launchd() {
    assert!(needs_kickstart(health_str::DOWN, ManageStrategy::Launchd));
    assert!(!needs_kickstart(
        health_str::HEALTHY,
        ManageStrategy::Launchd
    ));
    assert!(!needs_kickstart(
        health_str::NOT_INSTALLED,
        ManageStrategy::Launchd
    ));
    assert!(!needs_kickstart(health_str::DOWN, ManageStrategy::OwnVerb));
    assert!(!needs_kickstart(health_str::DOWN, ManageStrategy::None));
}

/// Why: the JSON envelope is a contract; pin its shape.
/// What: builds a report and asserts the keys.
/// Test: This is the test.
#[test]
fn report_serialises() {
    let report = VerifyTailReport::build(true, vec![row("trusty-search", "healthy")]);
    let v = serde_json::to_value(&report).expect("serialises");
    assert_eq!(v["command"], "install.verify");
    assert_eq!(v["ensure_ok"], true);
    assert_eq!(v["verified"], true);
    assert_eq!(v["members"][0]["member"], "trusty-search");
}

/// Why: THE #2560 safety property — a down/missing daemon OR a failed
/// ensure pass must both flip `verified` to `false`; only when BOTH are
/// clean is the install considered verified.
/// What: exercises each failure axis independently.
/// Test: This is the test.
#[test]
fn verified_requires_ensure_and_health() {
    let ensure_failed = VerifyTailReport::build(false, vec![row("trusty-search", "healthy")]);
    assert!(!ensure_failed.verified, "a failed ensure must not verify");

    let health_failed = VerifyTailReport::build(true, vec![row("trusty-search", "down")]);
    assert!(!health_failed.verified, "a down daemon must not verify");

    let not_installed =
        VerifyTailReport::build(true, vec![row("trusty-search", health_str::NOT_INSTALLED)]);
    assert!(!not_installed.verified, "not_installed must not verify");

    let both_ok = VerifyTailReport::build(true, vec![row("trusty-search", "healthy")]);
    assert!(both_ok.verified);
}

/// Why: `stale` (running, below version floor) and `unknown`
/// (process-managed, unprobeable — trusty-mpm) must NOT fail verification
/// — mirrors `stack::health::HealthReport`'s degrade policy exactly.
/// What: a report with only stale/unknown members still verifies.
/// Test: This is the test.
#[test]
fn verified_tolerates_stale_and_unknown() {
    let report = VerifyTailReport::build(
        true,
        vec![
            row("trusty-search", health_str::STALE),
            row("trusty-mpm", health_str::UNKNOWN),
        ],
    );
    assert!(report.verified);
}

/// Why: THE demo-critical fix — an OPTIONAL daemon member (e.g.
/// `trusty-console` never installed because it has no prebuilt for this
/// platform) being `down`/`not_installed` must NOT fail `verified`; only
/// a REQUIRED member's bad health may.
/// What: a report mixing a healthy REQUIRED member with a `down` and a
/// `not_installed` OPTIONAL member still verifies; a `down` REQUIRED
/// member alongside a healthy OPTIONAL one does not.
/// Test: This is the test.
#[test]
fn verified_ignores_optional_down_member() {
    let degraded = VerifyTailReport::build(
        true,
        vec![
            row("trusty-search", health_str::HEALTHY),
            optional_row("trusty-console", health_str::DOWN),
            optional_row("tga", health_str::NOT_INSTALLED),
        ],
    );
    assert!(
        degraded.verified,
        "an optional member's bad health must not fail verification"
    );

    let genuinely_failed = VerifyTailReport::build(
        true,
        vec![
            row("trusty-search", health_str::DOWN),
            optional_row("trusty-console", health_str::HEALTHY),
        ],
    );
    assert!(
        !genuinely_failed.verified,
        "a required member's bad health must still fail verification"
    );
}

/// Why: `print_human`'s colour path must never panic regardless of the
/// harness's stdout fd; this just confirms it is callable.
/// What: calls `use_color`, binds the result.
/// Test: This is the test.
#[test]
fn use_color_returns_bool() {
    let _v: bool = use_color();
}

/// Why: #3797 critic finding (MEDIUM, demo-relevant) — an OPTIONAL daemon
/// that is `not_installed` must read as "(optional, skipped)", not a
/// bare, alarming `not_installed` right above the green `VERIFIED` line.
/// A REQUIRED member's `not_installed` must get NO such softening
/// annotation, and a successful `kickstarted` retry must win over it.
/// (#3833: a bare `down` health with no `down_state` set no longer
/// happens in production — `verify_one` always attaches a `down_state`
/// for a down LAUNCHD member — so the `down`-specific cases moved to
/// `optional_annotation_reports_down_state` below.)
/// What: exercises the `not_installed` + `kickstarted` truth table.
/// Test: This is the test.
#[test]
fn optional_annotation_covers_not_installed_and_down() {
    assert_eq!(
        optional_annotation(&optional_row("trusty-console", health_str::NOT_INSTALLED)),
        " (optional, skipped)"
    );
    assert_eq!(
        optional_annotation(&optional_row("trusty-console", health_str::HEALTHY)),
        "",
        "a healthy optional member needs no annotation"
    );
    assert_eq!(
        optional_annotation(&row("trusty-search", health_str::NOT_INSTALLED)),
        "",
        "a REQUIRED member's not_installed health must not be softened"
    );
    let kickstarted = VerifyRow {
        kickstarted: true,
        health: health_str::HEALTHY.to_owned(),
        ..optional_row("trusty-console", health_str::HEALTHY)
    };
    assert_eq!(
        optional_annotation(&kickstarted),
        " (kickstarted)",
        "a kickstart retry that came back healthy is noted"
    );
}

/// Why: #3833 — a member still `down` after the poll wait must report
/// WHICH of the three end states it is, for both REQUIRED (plain
/// diagnosis, no softening) and OPTIONAL (still prefixed `optional,` —
/// it is informational, not an alarm) members. This must take priority
/// over the plain `kickstarted` note, since `down_state` being `Some`
/// means the kickstart did NOT resolve the problem.
/// What: exercises all three `DownState` variants for both required and
/// optional members, and confirms `down_state` wins over `kickstarted`.
/// Test: This is the test.
#[test]
fn optional_annotation_reports_down_state() {
    let with_state = |required: bool, state: DownState| VerifyRow {
        required,
        down_state: Some(state),
        ..row("trusty-memory", health_str::DOWN)
    };

    assert_eq!(
        optional_annotation(&with_state(true, DownState::NotLoaded)),
        " (not loaded)"
    );
    assert_eq!(
        optional_annotation(&with_state(false, DownState::NotLoaded)),
        " (optional, not loaded)"
    );
    assert_eq!(
        optional_annotation(&with_state(true, DownState::Crashed { exit_code: 2 })),
        " (crashed, exit 2)"
    );
    assert_eq!(
        optional_annotation(&with_state(false, DownState::Crashed { exit_code: -9 })),
        " (optional, crashed, exit -9)"
    );
    assert_eq!(
        optional_annotation(&with_state(true, DownState::StillStarting)),
        " (still starting)"
    );

    let kickstarted_but_still_down = VerifyRow {
        kickstarted: true,
        ..with_state(true, DownState::StillStarting)
    };
    assert_eq!(
        optional_annotation(&kickstarted_but_still_down),
        " (still starting)",
        "an unresolved down_state must win over the plain kickstarted note"
    );
}

/// Why: THE #3841 layer-2 safety property — the verify tail's own
/// bootstrap-fallback attempt must be visible in the human summary both
/// when it RESOLVED the member (down_state cleared back to `None`) and
/// when it was attempted but the diagnosis persists (fallback also
/// failed, or launchd still didn't pick up the label).
/// What: `verify_bootstrapped: true` with `down_state: None` reports
/// "(bootstrapped by installer)"; `verify_bootstrapped: true` with a
/// still-`Some` `down_state` appends ", fallback attempted" to the
/// existing diagnosis phrase; `verify_bootstrapped: false` (the default)
/// changes nothing versus the pre-#3841 behaviour.
/// Test: This is the test.
#[test]
fn optional_annotation_reports_verify_bootstrapped() {
    let resolved = VerifyRow {
        verify_bootstrapped: true,
        health: health_str::HEALTHY.to_owned(),
        ..row("trusty-memory", health_str::HEALTHY)
    };
    assert_eq!(
        optional_annotation(&resolved),
        " (bootstrapped by installer)"
    );

    let still_broken = VerifyRow {
        verify_bootstrapped: true,
        down_state: Some(DownState::NotLoaded),
        ..row("trusty-memory", health_str::DOWN)
    };
    assert_eq!(
        optional_annotation(&still_broken),
        " (not loaded, fallback attempted)"
    );

    let optional_still_broken = VerifyRow {
        required: false,
        ..still_broken.clone()
    };
    assert_eq!(
        optional_annotation(&optional_still_broken),
        " (optional, not loaded, fallback attempted)"
    );
}

/// In-memory [`ServiceEnv`](super::super::service_bootstrap::ServiceEnv)
/// fake for [`apply_not_loaded_fallback`]'s own tests (#3849 code-critic
/// MEDIUM 2) — records `bootstrap_fallback` calls and simulates plist
/// presence, so these tests never touch launchd or the real
/// `~/Library/LaunchAgents`. `is_loaded`/`run_service_install` are unused
/// by this layer but required by the trait; stubbed to inert values.
struct FakeServiceEnv {
    present: bool,
    fail_fallback: bool,
    fallback_calls: std::cell::RefCell<Vec<String>>,
}

impl super::super::service_bootstrap::ServiceEnv for FakeServiceEnv {
    fn plist_present(&self, _binary: &str) -> bool {
        self.present
    }
    fn run_service_install(&self, _binary: &str) -> anyhow::Result<()> {
        Ok(())
    }
    fn is_loaded(&self, _binary: &str) -> bool {
        false
    }
    fn bootstrap_fallback(&self, binary: &str) -> anyhow::Result<()> {
        self.fallback_calls.borrow_mut().push(binary.to_owned());
        if self.fail_fallback {
            anyhow::bail!("simulated fallback failure");
        }
        Ok(())
    }
}

/// Why: THE #3849 code-critic MEDIUM 2 core coverage gap — scenario (a):
/// `NotLoaded` + plist present, fallback SUCCEEDS. Must re-poll (not a
/// single instant probe — MEDIUM 1) until healthy, then clear
/// `down_state` back to `None` so the row (and the exit code) reflect the
/// repair. Asserting `waits > 0` is what proves the bounded POLL
/// machinery ran, not a single instant re-probe.
/// What: a probe sequence [down, down, healthy] must resolve to
/// `HEALTHY`/`None`/`attempted: true`, with at least one recorded wait.
/// Test: This is the test.
#[test]
fn apply_not_loaded_fallback_heals_after_poll_when_fallback_succeeds() {
    let env = FakeServiceEnv {
        present: true,
        fail_fallback: false,
        fallback_calls: std::cell::RefCell::new(Vec::new()),
    };
    let responses = std::cell::RefCell::new(vec![
        health_str::HEALTHY.to_owned(),
        health_str::DOWN.to_owned(),
        health_str::DOWN.to_owned(),
    ]);
    let waits = std::cell::RefCell::new(0u32);
    let (health, down_state, attempted) = apply_not_loaded_fallback(
        &env,
        "trusty-memory",
        ManageStrategy::Launchd,
        Duration::from_secs(30),
        || responses.borrow_mut().pop().expect("probed too many times"),
        || *waits.borrow_mut() += 1,
        || panic!("classify must not be called once health is no longer down"),
    );
    assert_eq!(health, health_str::HEALTHY);
    assert_eq!(down_state, None);
    assert!(attempted);
    assert_eq!(env.fallback_calls.borrow().as_slice(), ["trusty-memory"]);
    assert!(
        *waits.borrow() > 0,
        "must poll (wait between probes), never a single instant re-probe"
    );
}

/// Why: THE #3849 code-critic MEDIUM 2 core coverage gap — scenario (b):
/// `NotLoaded` + plist present, but the fallback bootstrap ITSELF fails.
/// Must report `verify_bootstrapped: true` (it WAS attempted) while
/// `health`/`down_state` stay exactly `down`/`NotLoaded` — no poll, no
/// classify call, no false "healed" signal — so `verified`/the exit code
/// still reflects genuine failure.
/// What: asserts `health == "down"`, `down_state == Some(NotLoaded)`,
/// `attempted == true`, and that `probe`/`wait`/`classify` were never
/// invoked (they panic if called).
/// Test: This is the test.
#[test]
fn apply_not_loaded_fallback_reports_still_not_loaded_when_fallback_fails() {
    let env = FakeServiceEnv {
        present: true,
        fail_fallback: true,
        fallback_calls: std::cell::RefCell::new(Vec::new()),
    };
    let (health, down_state, attempted) = apply_not_loaded_fallback(
        &env,
        "trusty-review",
        ManageStrategy::Launchd,
        Duration::from_secs(30),
        || panic!("probe must not run when the fallback bootstrap itself failed"),
        || panic!("wait must not run when the fallback bootstrap itself failed"),
        || panic!("classify must not run when the fallback bootstrap itself failed"),
    );
    assert_eq!(health, health_str::DOWN);
    assert_eq!(down_state, Some(DownState::NotLoaded));
    assert!(
        attempted,
        "the fallback WAS attempted, even though it failed"
    );
}

/// Why: a `NotLoaded` diagnosis with NO plist on disk at all is a genuine
/// "never installed" state `launchctl bootstrap` cannot repair (nothing
/// to load) — must be a true no-op: `verify_bootstrapped: false`, health/
/// down_state untouched, and neither the fallback nor probe/wait/classify
/// ever invoked.
/// What: `present: false` yields `attempted: false` and zero side effects.
/// Test: This is the test.
#[test]
fn apply_not_loaded_fallback_noop_when_no_plist() {
    let env = FakeServiceEnv {
        present: false,
        fail_fallback: false,
        fallback_calls: std::cell::RefCell::new(Vec::new()),
    };
    let (health, down_state, attempted) = apply_not_loaded_fallback(
        &env,
        "trusty-console",
        ManageStrategy::Launchd,
        Duration::from_secs(30),
        || panic!("probe must not run when there is no plist to bootstrap"),
        || panic!("wait must not run when there is no plist to bootstrap"),
        || panic!("classify must not run when there is no plist to bootstrap"),
    );
    assert_eq!(health, health_str::DOWN);
    assert_eq!(down_state, Some(DownState::NotLoaded));
    assert!(!attempted);
    assert!(env.fallback_calls.borrow().is_empty());
}

/// Why: THE #3849 code-critic MEDIUM 1 edge case — an exhausted
/// `remaining_budget` at the moment the fallback succeeds must still
/// re-probe (never just trust the fallback's exit code), but as a SINGLE
/// instant probe rather than an unbounded/over-budget poll.
/// What: `remaining_budget: Duration::ZERO` with a probe that reports
/// healthy immediately still resolves `HEALTHY`/`None`, but `wait` is
/// never called.
/// Test: This is the test.
#[test]
fn apply_not_loaded_fallback_uses_instant_probe_when_budget_exhausted() {
    let env = FakeServiceEnv {
        present: true,
        fail_fallback: false,
        fallback_calls: std::cell::RefCell::new(Vec::new()),
    };
    let waits = std::cell::RefCell::new(0u32);
    let (health, down_state, attempted) = apply_not_loaded_fallback(
        &env,
        "trusty-memory",
        ManageStrategy::Launchd,
        Duration::ZERO,
        || health_str::HEALTHY.to_owned(),
        || *waits.borrow_mut() += 1,
        || panic!("classify must not run once the instant probe reports healthy"),
    );
    assert_eq!(health, health_str::HEALTHY);
    assert_eq!(down_state, None);
    assert!(attempted);
    assert_eq!(
        *waits.borrow(),
        0,
        "an exhausted budget must use a single instant probe, never wait/poll"
    );
}

/// Why: THE #3833 safety property — polling must terminate the instant
/// health stops being `down`, without over-waiting or under-waiting.
/// What: a probe sequence [down, down, healthy] must return after
/// exactly 3 attempts with 2 waits.
/// Test: This is the test.
#[test]
fn poll_until_not_down_stops_when_healthy() {
    let responses = std::cell::RefCell::new(vec![
        health_str::HEALTHY.to_owned(),
        health_str::DOWN.to_owned(),
        health_str::DOWN.to_owned(),
    ]);
    let waits = std::cell::RefCell::new(0u32);
    let (health, attempts) = poll_until_not_down(
        || {
            responses
                .borrow_mut()
                .pop()
                .expect("probe called too many times")
        },
        || *waits.borrow_mut() += 1,
        10,
    );
    assert_eq!(health, health_str::HEALTHY);
    assert_eq!(attempts, 3);
    assert_eq!(*waits.borrow(), 2, "must wait exactly twice, not 3 times");
}

/// Why: against a genuinely dead daemon, the poll must still terminate —
/// never block `tctl install` forever.
/// What: a probe that always returns `down` must stop at exactly
/// `max_attempts`, waiting `max_attempts - 1` times (never after the
/// final attempt).
/// Test: This is the test.
#[test]
fn poll_until_not_down_stops_at_max_attempts() {
    let waits = std::cell::RefCell::new(0u32);
    let (health, attempts) = poll_until_not_down(
        || health_str::DOWN.to_owned(),
        || *waits.borrow_mut() += 1,
        4,
    );
    assert_eq!(health, health_str::DOWN);
    assert_eq!(attempts, 4);
    assert_eq!(
        *waits.borrow(),
        3,
        "must not wait after the final (4th) attempt"
    );
}

/// Why: the common case — a daemon that is already healthy by the time
/// the retry fires — must return immediately with zero waits, not
/// needlessly sleep once.
/// What: a probe that is healthy on the first call triggers no wait.
/// Test: This is the test.
#[test]
fn poll_until_not_down_first_attempt_needs_no_wait() {
    let waits = std::cell::RefCell::new(0u32);
    let (health, attempts) = poll_until_not_down(
        || health_str::HEALTHY.to_owned(),
        || *waits.borrow_mut() += 1,
        10,
    );
    assert_eq!(health, health_str::HEALTHY);
    assert_eq!(attempts, 1);
    assert_eq!(*waits.borrow(), 0);
}

/// Why: #3836 MEDIUM fix — with a generous remaining budget, a member
/// must still get its full [`POLL_MAX_ATTEMPTS`] ceiling, never MORE.
/// What: a large `remaining` (e.g. the full aggregate budget) clamps to
/// exactly `POLL_MAX_ATTEMPTS`.
/// Test: This is the test.
#[test]
fn bounded_attempts_clamped_to_max() {
    assert_eq!(bounded_attempts(AGGREGATE_POLL_BUDGET), POLL_MAX_ATTEMPTS);
    assert_eq!(
        bounded_attempts(Duration::from_secs(3600)),
        POLL_MAX_ATTEMPTS
    );
}

/// Why: THE #3836 core safety property — a member turn late in the fold,
/// with little aggregate budget left, must get proportionally FEWER
/// attempts, not the full schedule (which is what let a single member
/// blow through the aggregate cap).
/// What: a small remaining budget yields fewer than `POLL_MAX_ATTEMPTS`
/// attempts, and a larger remaining budget yields a value in between.
/// Test: This is the test.
#[test]
fn bounded_attempts_shrinks_with_small_budget() {
    // 12s remaining / 5s interval -> 1 + 2 = 3 attempts.
    assert_eq!(bounded_attempts(Duration::from_secs(12)), 3);
    // 30s remaining -> 1 + 6 = 7 attempts, strictly between 1 and the max.
    let mid = bounded_attempts(Duration::from_secs(30));
    assert!(
        mid > 1 && mid < POLL_MAX_ATTEMPTS,
        "expected a value strictly between 1 and {POLL_MAX_ATTEMPTS}, got {mid}"
    );
}

/// Why: even a sliver of remaining budget must still attempt AT LEAST
/// one probe — never zero (a zero-attempt poll makes no sense; an
/// entirely exhausted budget is handled as its own separate case by
/// `verify_one`, not by this function receiving a zero `remaining`).
/// What: a 1-second remaining budget still yields `1`.
/// Test: This is the test.
#[test]
fn bounded_attempts_at_least_one() {
    assert_eq!(bounded_attempts(Duration::from_secs(1)), 1);
}

/// Why: #3836 — the "budget exhausted" summary note must fire iff ANY
/// member's poll was skipped for that reason, and must NOT fire on an
/// all-clear report (avoids alarming an operator over nothing).
/// What: exercises both the empty/all-false case and a mixed case.
/// Test: This is the test.
#[test]
fn any_budget_exhausted_detects_any_row() {
    assert!(!any_budget_exhausted(&[]));
    assert!(!any_budget_exhausted(&[row("trusty-search", "healthy")]));

    let exhausted = VerifyRow {
        budget_exhausted: true,
        ..row("trusty-memory", health_str::DOWN)
    };
    assert!(any_budget_exhausted(&[
        row("trusty-search", "healthy"),
        exhausted
    ]));
}
