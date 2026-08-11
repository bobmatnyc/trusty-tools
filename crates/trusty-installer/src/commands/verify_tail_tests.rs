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

/// A [`StableMember`] shaped like the launchd daemons #4246 is about.
///
/// Why: the two `verify_one_with` tests need a member whose strategy is
/// `Launchd` (so the kickstart branch is reachable) and whose binary is
/// resolvable but has no documented port or live daemon — see
/// `test_support::PROBEABLE_BINARY` for why that combination is required.
fn launchd_member(binary: &str) -> StableMember {
    StableMember {
        crate_name: binary.to_owned(),
        binary: binary.to_owned(),
        daemon: true,
        manage: ManageStrategy::Launchd,
        required: true,
    }
}

/// Recording [`Kickstarter`] fake (#4246) — the seam that makes "did this
/// restart a healthy daemon?" an assertable question. Never touches `launchctl`.
#[derive(Default)]
struct FakeKickstarter {
    calls: std::cell::RefCell<Vec<String>>,
}

impl Kickstarter for FakeKickstarter {
    fn kickstart(&self, binary: &str) -> bool {
        self.calls.borrow_mut().push(binary.to_owned());
        true
    }
}

/// Why: only a CONFIRMED-down LAUNCHD daemon is the #2498 failure signature.
/// #4246 changed the input from a hand-typed string to a typed `ProbeOutcome`
/// and the condition to `is_confirmed_down` — a transport-level observation.
/// What: asserts the truth table across outcome x manage-strategy, including the
/// mirror property that a genuinely-refusing daemon IS still repaired.
/// Test: This is the test.
#[test]
fn needs_kickstart_only_for_confirmed_down_launchd() {
    for confirmed in [ProbeOutcome::Refused, ProbeOutcome::Timeout] {
        assert!(
            needs_kickstart(&confirmed, ManageStrategy::Launchd),
            "{confirmed:?} on a launchd member must still be repaired"
        );
        assert!(!needs_kickstart(&confirmed, ManageStrategy::OwnVerb));
        assert!(!needs_kickstart(&confirmed, ManageStrategy::None));
    }

    let serving = ProbeOutcome::Serving {
        status: "ok".to_owned(),
        version: None,
    };
    assert!(!needs_kickstart(&serving, ManageStrategy::Launchd));
    assert!(!needs_kickstart(
        &ProbeOutcome::NotInstalled,
        ManageStrategy::Launchd
    ));
    assert!(!needs_kickstart(
        &ProbeOutcome::Unprobeable,
        ManageStrategy::Launchd
    ));
}

/// Why: THE #4246 gate. A schema mismatch, an unusable response, a squatter on a
/// documented port, or a local probe failure all mean *something* answered (or
/// that we never looked) — restarting on any of them is a guess, and
/// `kickstart -k` costs trusty-search's unflushed HNSW vectors. Before the fix
/// every one of these was literally the string `"down"` and fired the restart.
/// What: asserts each non-transport outcome is refused a kickstart even on a
/// `Launchd` member — the strategy that CAN be kickstarted.
/// Test: This is the test.
#[test]
fn needs_kickstart_never_fires_on_a_schema_problem() {
    for benign in [
        ProbeOutcome::BadEnvelope {
            got: r#"{"daemon":"running"}"#.to_owned(),
        },
        ProbeOutcome::HttpError { status: 500 },
        ProbeOutcome::NoAddress,
        ProbeOutcome::ProbeFailed {
            detail: "no runtime".to_owned(),
        },
        ProbeOutcome::Serving {
            status: "degraded".to_owned(),
            version: None,
        },
    ] {
        assert!(
            !needs_kickstart(&benign, ManageStrategy::Launchd),
            "{benign:?} must never authorise `launchctl kickstart -k`"
        );
    }
}

/// Why: **THE regression test for #4246's actual harm.** Every `tctl install`
/// hard-restarted a fully healthy stack, because `probe_member_health` shelled
/// out to a `health --json` verb no daemon implements, read the failure as
/// `down`, and `verify_one` kickstarted on it. The pre-existing
/// `needs_kickstart_only_for_down_launchd` passed the whole time, because it fed
/// the predicate a hand-typed `"down"` — the predicate was never wrong; the probe
/// manufactured its input. Only a test that runs the REAL probe against a stubbed
/// healthy daemon and then asserts the restart decision closes that loop.
///
/// What: plants a real `http_addr` pointing at a stub answering
/// `200 {"status":"ok"}`, runs `verify_one_with` for a `Launchd` member with the
/// full poll budget, and asserts ZERO kickstart calls — plus a `healthy` row with
/// no down-state diagnosis and no bootstrap fallback, so no `launchctl` was
/// touched at all.
/// Test: This is the test.
#[test]
fn verify_one_does_not_kickstart_a_healthy_launchd_daemon() {
    use crate::commands::test_support as ts;
    let _guard = ts::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let addr = ts::stub_seq_blocking(vec![(
        "HTTP/1.1 200 OK",
        r#"{"status":"ok","version":"0.39.0"}"#,
    )]);
    let dir = ts::stub_data_dir(ts::PROBEABLE_BINARY, &addr);

    let kicks = FakeKickstarter::default();
    let row = verify_one_with(
        &launchd_member(ts::PROBEABLE_BINARY),
        AGGREGATE_POLL_BUDGET,
        &kicks,
    );
    ts::clear_data_dir_override(&dir);

    assert!(
        kicks.calls.borrow().is_empty(),
        "a healthy daemon must NEVER be kickstarted — got {:?}",
        kicks.calls.borrow()
    );
    assert_eq!(row.health, health_str::HEALTHY);
    assert!(!row.kickstarted);
    assert_eq!(row.down_state, None, "a healthy member needs no diagnosis");
    assert!(!row.verify_bootstrapped);
    assert!(!row.budget_exhausted);
}

/// Why: the mirror of the test above, so the fix cannot degenerate into "never
/// repair anything" — the #2498 failure signature (`launchctl bootstrap`
/// succeeded, `RunAtLoad` never fired) must STILL be repaired. This is the
/// regression that a naive "gate on confirmed-down without changing the
/// transport" would have shipped: an outage that persists.
///
/// What: plants an `http_addr` pointing at a released ephemeral port (so the
/// probe genuinely observes `Refused`), and asserts exactly one kickstart for
/// this member. `remaining_budget` is 1s deliberately: `bounded_attempts(1s)` is
/// 1, so the post-kickstart poll takes exactly one more probe and ZERO
/// `POLL_INTERVAL` sleeps, keeping the test fast. The trailing `NotLoaded`
/// diagnosis comes from a read-only `launchctl list` against a label that exists
/// nowhere, and the #3841 fallback is a no-op because no plist is present — so
/// nothing here bootstraps or restarts a real service.
/// Test: This is the test.
#[test]
fn verify_one_kickstarts_a_genuinely_down_launchd_daemon() {
    use crate::commands::test_support as ts;
    let _guard = ts::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = ts::stub_data_dir(ts::PROBEABLE_BINARY, &ts::dead_addr());

    let kicks = FakeKickstarter::default();
    let row = verify_one_with(
        &launchd_member(ts::PROBEABLE_BINARY),
        Duration::from_secs(1),
        &kicks,
    );
    ts::clear_data_dir_override(&dir);

    assert_eq!(
        kicks.calls.borrow().as_slice(),
        [ts::PROBEABLE_BINARY.to_owned()],
        "a genuinely refusing daemon must still get its one #2498 kickstart"
    );
    assert!(row.kickstarted);
    assert_eq!(row.health, health_str::DOWN);
    assert_eq!(
        row.down_state,
        Some(DownState::NotLoaded),
        "an unloaded label diagnoses as NotLoaded on every platform"
    );
    assert!(
        !row.verify_bootstrapped,
        "with no plist on disk the #3841 fallback must not run"
    );
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
    /// #4470: when `true`, the port guard refuses this member.
    refuse_port: bool,
}

impl super::super::service_bootstrap::ServiceEnv for FakeServiceEnv {
    fn plist_present(&self, _binary: &str) -> bool {
        self.present
    }
    fn run_service_install(
        &self,
        _binary: &str,
        _exe_path: Option<&std::path::Path>,
    ) -> anyhow::Result<()> {
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
    fn port_guard(&self, _binary: &str) -> Result<(), String> {
        if self.refuse_port {
            Err("port 7878 is held by pid 9931, which launchd does not supervise".to_owned())
        } else {
            Ok(())
        }
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
        refuse_port: false,
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
        refuse_port: false,
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
        refuse_port: false,
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
        refuse_port: false,
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

/// Why (#4470): `attempt_verify_fallback` is the THIRD site in this crate that
/// issues a `launchctl bootstrap`, and #3841's lesson is that a guard applied
/// to only some branches leaves exactly the damaged machines on an ungated one.
/// A bootstrap into a port a foreign process already owns exits 0 and reports a
/// repair that did not happen.
///
/// This test fails if the guard is removed from that function: without it the
/// fallback runs, `fallback_calls` records `trusty-memory`, and the return value
/// becomes `Some(true)` instead of `None`.
///
/// What: with the plist present but the port held by an unsupervised process,
/// asserts nothing was attempted and no bootstrap was issued.
/// Test: This is the test.
#[test]
fn attempt_verify_fallback_refuses_on_foreign_port() {
    let env = FakeServiceEnv {
        present: true,
        fail_fallback: false,
        fallback_calls: std::cell::RefCell::new(Vec::new()),
        refuse_port: true,
    };
    assert_eq!(
        attempt_verify_fallback(&env, "trusty-memory"),
        None,
        "a refused port guard must report NOTHING attempted"
    );
    assert!(
        env.fallback_calls.borrow().is_empty(),
        "a refusal must not issue a `launchctl bootstrap`"
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
