//! Post-install verify tail: `ensure` + health, with the #2498 kickstart retry
//! (#2560) and the #3833 poll-until-ready wait.
//!
//! Why: `tctl install` placed binaries (and, when enabled, bootstrapped launchd
//! services) but never confirmed the stack actually came up — an operator
//! following the `curl | sh` one-liner had no automated signal that a daemon
//! silently stayed down (the exact "looks installed, actually broken" failure
//! class #2557 / #2498 exist because of). This module runs immediately after
//! `install_all` (skipped entirely by `--no-verify`) and its result is folded
//! into the SAME [`super::install::InstallReport`] so the exit code CI/automation
//! gates on already reflects it.
//!
//! What: [`run_verify_tail`]:
//! 1. Runs the `.mcp.json` patch + trusty-search index / trusty-memory palace
//!    registration — the same logic `tctl ensure` runs (reused directly via
//!    [`super::ensure::mcp_patch`] / [`super::ensure::project_setup`], not
//!    reimplemented; only the CLI-level render is skipped so `--json` install
//!    output stays a single JSON object).
//! 2. Probes health for every selected daemon member.
//! 3. For any `down` LAUNCHD daemon (the #2498 failure signature: `launchctl
//!    bootstrap` succeeds but `RunAtLoad` doesn't fire), issues one
//!    `launchctl kickstart -k gui/<uid>/<label>` and then, instead of a single
//!    instant re-probe (#3833 — this raced trusty-search's first-boot
//!    embedding-model download and reported a false `NOT VERIFIED`),
//!    [`poll_until_not_down`] re-probes on a bounded ~60s schedule so a
//!    slow-but-healthy daemon is given real wall-clock time to come up before
//!    the verdict is finalised.
//! 4. A member still `down` after that wait is classified into one of three
//!    distinct end states via `super::verify_launchd_state::classify_down_state`
//!    — [`DownState::NotLoaded`], [`DownState::Crashed`], or
//!    [`DownState::StillStarting`] — instead of a uniform, underspecified
//!    `down` (#3833; the diagnosis machinery itself lives in the sibling
//!    `verify_launchd_state` module, shared with `service_bootstrap`'s #3836
//!    defensive-fallback check).
//! 5. (#3841 belt-and-braces, second layer) A member classified
//!    [`DownState::NotLoaded`] whose plist is present on disk gets ONE more
//!    repair attempt here — [`apply_not_loaded_fallback`] calls the exact same
//!    `ServiceEnv::bootstrap_fallback` primitive
//!    `service_bootstrap::bootstrap_one`'s install-time postcondition uses
//!    (via [`attempt_verify_fallback`]), and on success re-probes through the
//!    SAME [`poll_until_not_down`]/[`bounded_attempts`] machinery the #2498
//!    kickstart retry uses (#3849 code-critic MEDIUM 1 fix — a single instant
//!    re-probe here would race a just-repaired but slow-starting daemon
//!    exactly like the #3833 kickstart race did) — bounded by whatever of
//!    `remaining_budget` the member's earlier kickstart poll (if any) hasn't
//!    already spent. This exists so that even a FUTURE skip-path regression
//!    in the install-time layer (or a member the install-time bootstrap never
//!    touched this run, e.g. `--no-service`) still self-heals at verify time
//!    instead of silently reporting `NOT VERIFIED`.
//!
//! Because `run_verify_tail` folds [`verify_one`] SEQUENTIALLY over every
//! selected daemon member, and each member's poll wait can run up to
//! [`POLL_MAX_ATTEMPTS`] * [`POLL_INTERVAL`] (~55s), an aggregate
//! [`AGGREGATE_POLL_BUDGET`] (#3836 MEDIUM fix) caps the TOTAL time spent
//! polling across ALL members (~120s) — without it, five simultaneously-stuck
//! daemons would stall `tctl install` for ~4-5 minutes, exactly when a fast,
//! actionable signal matters most. Once the aggregate budget is exhausted,
//! remaining members skip their poll wait entirely (an instant probe only)
//! and their row is flagged `budget_exhausted` — surfaced as one summary note
//! in [`print_human`], not repeated per row.
//!
//! [`print_human`] renders the final green/red pass/fail summary — the last
//! thing the installer prints. Because the health used to build the report is
//! already the POST-wait value, the exit code CI/automation gates on
//! ([`super::install::InstallReport`], folded from `verified`) reflects the
//! final state, not the instant-after-kickstart snapshot.
//!
//! Test: the pure decision pieces (`needs_kickstart`, `poll_until_not_down`,
//! `bounded_attempts`, the report's `verified` derivation) are unit-tested
//! here; `verify_launchd_state`'s own module doc covers its
//! `classify_down_state_from_entry`/`parse_launchd_list_text` tests. The
//! `ensure` phase and the `launchctl` subprocess calls are side-effecting and
//! validated manually.

use std::time::{Duration, Instant};

use serde::Serialize;

use super::probe::{health_str, probe_member_health};
use super::stable_set::{ManageStrategy, StableMember};
use super::verify_launchd_state::{classify_down_state, DownState};

/// Per-member poll interval — how long [`poll_until_not_down`] sleeps between
/// re-probes (#3833).
///
/// Why: trusty-search downloads embedding models on first boot and can take
/// tens of seconds; a single instant re-probe (the pre-#3833 behaviour) raced
/// that window.
const POLL_INTERVAL: Duration = Duration::from_secs(5);

/// Per-member cap on probe attempts (one immediately, then one every
/// [`POLL_INTERVAL`]) when the AGGREGATE budget allows it in full — `12`
/// attempts spread over 11 intervals is ~55s, within the ~30-60s per-member
/// budget this crate's issue/CHANGELOG describe. [`bounded_attempts`] may
/// return fewer when the aggregate budget is running low (#3836).
const POLL_MAX_ATTEMPTS: u32 = 12;

/// Aggregate wall-clock budget for the ENTIRE poll-wait phase across ALL
/// members in one `run_verify_tail` call (#3836 MEDIUM fix).
///
/// Why: `verify_one`'s per-member poll (up to [`POLL_MAX_ATTEMPTS`] *
/// [`POLL_INTERVAL`] ≈ 55s) folds SEQUENTIALLY over up to five launchd
/// members in `run_verify_tail` — with every member simultaneously stuck,
/// that is a ~4-5 minute worst-case stall, exactly when multiple daemons
/// failing to start is the scenario an operator most needs a fast,
/// actionable signal for. Capping the AGGREGATE budget (not just the
/// per-member one) bounds the worst case while still giving a single
/// genuinely slow-but-healthy daemon its full per-member window when it's
/// the only one stuck.
const AGGREGATE_POLL_BUDGET: Duration = Duration::from_secs(120);

/// One member's verify-tail outcome.
///
/// Why: a typed row keeps the `--json` output stable + testable.
/// What: `member` crate name; `health` the (possibly kickstart-retried, POST
/// wait) health verdict string; `kickstarted` whether a #2498 kickstart retry
/// was attempted for this member; `required` (graceful-degrade, demo-critical
/// fix) mirrors `StableMember::required` and gates whether a down/not-installed
/// verdict for this member may fail the overall `verified` verdict;
/// `down_state` (#3833) is `Some` iff `health` is still `down` for a LAUNCHD
/// member after the wait, classifying WHY; `budget_exhausted` (#3836) is
/// `true` iff this member needed a poll wait but the AGGREGATE poll budget
/// had already run out, so NO poll was attempted for it (only the instant
/// probe); `verify_bootstrapped` (#3841) is `true` iff this row's `NotLoaded`
/// diagnosis triggered the verify tail's own second-layer bootstrap-fallback
/// attempt (regardless of whether that attempt succeeded — `down_state`/
/// `health` reflect the outcome).
/// Test: `tests::report_serialises`.
#[derive(Clone, Debug, Serialize)]
pub struct VerifyRow {
    /// Crate name.
    pub member: String,
    /// Health verdict after any kickstart retry + poll wait + (#3841) verify-
    /// tail bootstrap fallback.
    pub health: String,
    /// Whether a `launchctl kickstart -k` retry was attempted (#2498).
    pub kickstarted: bool,
    /// Whether this member is REQUIRED for the overall `verified` verdict.
    pub required: bool,
    /// Whether the AGGREGATE poll budget ([`AGGREGATE_POLL_BUDGET`]) was
    /// already exhausted by the time this member needed a poll wait, so it
    /// was skipped (#3836).
    pub budget_exhausted: bool,
    /// Why a LAUNCHD member is still `down` after the poll wait (and, when
    /// applicable, the #3841 verify-tail fallback), when it is (#3833).
    /// `None` for a healthy/stale/unknown/not_installed member, or a
    /// non-LAUNCHD member.
    pub down_state: Option<DownState>,
    /// Whether [`attempt_verify_fallback`] was invoked for this member
    /// (#3841 — the verify tail's own second-layer repair for a `NotLoaded`
    /// diagnosis, independent of `service_bootstrap`'s install-time
    /// postcondition).
    pub verify_bootstrapped: bool,
}

/// The aggregate verify-tail report (#2560).
///
/// Why: `--json` consumers need the ensure + health verdict as one object,
/// nested inside [`super::install::InstallReport`].
/// What: `ensure_ok` — whether the `.mcp.json` patch and project-setup stages
/// all succeeded; `members` — per-daemon health rows; `verified` — the overall
/// verdict this module derives (see [`VerifyTailReport::build`]).
/// Test: `tests::verified_requires_ensure_and_health`.
#[derive(Clone, Debug, Serialize)]
pub struct VerifyTailReport {
    /// Fixed command tag for JSON consumers.
    pub command: &'static str,
    /// Whether the `.mcp.json` patch + project-setup stages all succeeded.
    pub ensure_ok: bool,
    /// Per-daemon-member verify rows.
    pub members: Vec<VerifyRow>,
    /// Overall verdict: `ensure_ok` AND every REQUIRED member
    /// healthy/stale/unknown (never `down`/`not_installed`). An OPTIONAL
    /// member never fails this (graceful-degrade, demo-critical fix).
    pub verified: bool,
}

impl VerifyTailReport {
    /// Build a report and derive the overall `verified` verdict.
    ///
    /// Why: one place derives the verdict so the JSON and the folded
    /// `InstallReport.all_ok` can never disagree.
    /// What: `verified = ensure_ok AND no REQUIRED member is
    /// `down`/`not_installed`` (an OPTIONAL member's health never fails this
    /// — demo-critical fix). A `stale` or `unknown` member does NOT fail
    /// verification — mirrors `stack::health::HealthReport`'s degrade policy
    /// exactly (an under-the-version-floor daemon is still up; an
    /// unprobeable process-managed member is not a verified gap).
    /// Test: `tests::verified_requires_ensure_and_health`,
    /// `tests::verified_tolerates_stale_and_unknown`,
    /// `tests::verified_ignores_optional_down_member`.
    fn build(ensure_ok: bool, members: Vec<VerifyRow>) -> Self {
        let health_ok = members
            .iter()
            .filter(|m| m.required)
            .all(|m| m.health != health_str::DOWN && m.health != health_str::NOT_INSTALLED);
        Self {
            command: "install.verify",
            ensure_ok,
            verified: ensure_ok && health_ok,
            members,
        }
    }
}

/// Whether a member's health verdict warrants a #2498 kickstart retry.
///
/// Why: only a `down` LAUNCHD daemon is the #2498 failure signature
/// (`launchctl bootstrap` succeeded, `RunAtLoad` never fired); a
/// `not_installed` member, an `unknown` process-managed member (trusty-mpm),
/// or a non-launchd member must never trigger a kickstart attempt.
/// What: `true` iff `health == "down"` AND `manage == Launchd`.
/// Test: `tests::needs_kickstart_only_for_down_launchd`.
pub fn needs_kickstart(health: &str, manage: ManageStrategy) -> bool {
    health == health_str::DOWN && manage == ManageStrategy::Launchd
}

/// Run `launchctl kickstart -k gui/<uid>/<label>` for a member (macOS only).
///
/// Why: the #2498 recovery step — `launchctl bootstrap` can report success
/// while `RunAtLoad` never actually fires; `kickstart -k` force-starts the job.
/// What: resolves the uid + launchd label, spawns the command, returns whether
/// it exited successfully. On non-macOS, always returns `false` (no-op). A
/// `false` return does not by itself mean the daemon is unreachable — see
/// [`poll_until_not_down`], which is run regardless (#3833): a kickstart on a
/// label that IS loaded but slow to accept the force-start can transiently
/// fail here while the daemon still comes up moments later, and a kickstart
/// on a label that was NEVER bootstrapped fails outright (there is nothing to
/// force-start) — [`classify_down_state`] is what actually distinguishes
/// those cases in the final report, not this return value.
/// Test: side-effecting; not invoked in the test suite.
#[cfg(target_os = "macos")]
fn kickstart(binary: &str) -> bool {
    let uid = super::plist_bootstrap::resolve_uid();
    let label = super::plist_label::plist_label_for(binary);
    std::process::Command::new("launchctl")
        .args(["kickstart", "-k", &format!("gui/{uid}/{label}")])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(not(target_os = "macos"))]
fn kickstart(_binary: &str) -> bool {
    false
}

/// Poll `probe` until it stops reporting `down`, sleeping via `wait` between
/// attempts, up to `max_attempts` total probes (#3833).
///
/// Why: a single instant re-probe right after `launchctl kickstart` races a
/// daemon's real startup time — trusty-search notably downloads embedding
/// models on first boot and can take tens of seconds. Replacing the instant
/// check with a bounded poll gives a slow-but-healthy daemon real wall-clock
/// time to come up before the verdict is finalised, while still terminating
/// (never blocking `tctl install` indefinitely) against a genuinely dead
/// daemon. Kept generic over `probe`/`wait` (rather than calling
/// `probe_member_health`/`std::thread::sleep` directly) so the state machine
/// is unit-testable with fakes and NEVER sleeps in the test suite.
///
/// # Preconditions
/// `max_attempts >= 1`.
/// # Postconditions
/// Returns after at most `max_attempts` calls to `probe`; the returned
/// `attempts` is in `1..=max_attempts`; `wait` is called exactly
/// `attempts - 1` times (never after the final attempt); the returned health
/// string is either not equal to `health_str::DOWN`, or `attempts ==
/// max_attempts`.
/// Test: `tests::poll_until_not_down_stops_when_healthy`,
/// `tests::poll_until_not_down_stops_at_max_attempts`,
/// `tests::poll_until_not_down_first_attempt_needs_no_wait`.
fn poll_until_not_down<F, W>(mut probe: F, mut wait: W, max_attempts: u32) -> (String, u32)
where
    F: FnMut() -> String,
    W: FnMut(),
{
    debug_assert!(max_attempts >= 1, "max_attempts must be at least 1");
    let mut attempts = 0u32;
    loop {
        let health = probe();
        attempts += 1;
        if health != health_str::DOWN || attempts >= max_attempts {
            return (health, attempts);
        }
        wait();
    }
}

/// Clamp the per-member poll attempt count to fit within a `remaining`
/// AGGREGATE budget (#3836 MEDIUM fix), never exceeding [`POLL_MAX_ATTEMPTS`].
///
/// Why: without this, one member's poll always runs its full
/// [`POLL_MAX_ATTEMPTS`] schedule regardless of how much of the
/// [`AGGREGATE_POLL_BUDGET`] earlier members already consumed — the LAST
/// member in a fold of several simultaneously-stuck daemons could still
/// overrun the aggregate budget by a full ~55s. Sizing this member's OWN
/// attempt count to what's actually left keeps the aggregate fold close to
/// its budget without needing a fully async/interruptible poll loop.
///
/// # Preconditions
/// `remaining` is nonzero — callers must special-case an already-exhausted
/// budget separately (skip the poll entirely; see [`verify_one`]).
/// # Postconditions
/// Returns a value in `1..=POLL_MAX_ATTEMPTS`.
/// Test: `tests::bounded_attempts_*`.
fn bounded_attempts(remaining: Duration) -> u32 {
    debug_assert!(!remaining.is_zero(), "remaining budget must be nonzero");
    let interval_secs = POLL_INTERVAL.as_secs().max(1);
    let by_budget = 1 + (remaining.as_secs() / interval_secs) as u32;
    by_budget.clamp(1, POLL_MAX_ATTEMPTS)
}

/// Attempt the installer's own launchd bootstrap fallback for a member the
/// verify tail has just diagnosed as [`DownState::NotLoaded`] (#3841
/// belt-and-braces, second layer).
///
/// Why: `service_bootstrap::bootstrap_one`'s postcondition check is the FIRST
/// line of defense against #3832's failure signature (a `service install`
/// that writes a plist without loading it), but it only runs during install,
/// on whichever code path THIS run's `install_all` loop actually took for the
/// member. A future regression in that skip-path handling — or a member
/// `--no-service` opted the install-time bootstrap out of entirely — would
/// otherwise leave the verify tail reporting a `NotLoaded` diagnosis with no
/// repair attempt at all. This is that second, independent attempt, reusing
/// the EXACT same [`super::service_bootstrap::ServiceEnv::bootstrap_fallback`]
/// primitive so the two layers can never disagree about how a label gets
/// force-loaded.
///
/// 🔴 (#3849 code-critic MEDIUM 2 fix) Takes `env: &dyn ServiceEnv` — never
/// hardcodes `RealServiceEnv` itself — mirroring `service_bootstrap::
/// bootstrap_one`'s own seam so this whole decision is unit-testable against
/// an in-memory fake, the same way `bootstrap_one`'s is. The real call site
/// ([`apply_not_loaded_fallback_real`]) is the only place that names
/// `RealServiceEnv`.
/// What: if the member's plist exists on disk (`ServiceEnv::plist_present`),
/// calls `ServiceEnv::bootstrap_fallback` and returns `Some(true)`/`Some(false)`
/// for whether it succeeded. Returns `None` (nothing attempted) when the
/// plist is not present at all — that is a genuine "never installed" state a
/// `launchctl bootstrap` cannot repair (there is no plist to load), not this
/// failure mode.
/// Test: `tests::apply_not_loaded_fallback_*` (via `FakeServiceEnv`, composed
/// through [`apply_not_loaded_fallback`] below).
///
/// `#[cfg(any(test, target_os = "macos"))]`: the only PRODUCTION call site
/// ([`apply_not_loaded_fallback_real`]'s macOS branch) is itself macOS-only —
/// on a non-macOS build with `test` cfg off, nothing calls this function at
/// all, and an un-gated `fn` here would be flagged `dead_code` under `-D
/// warnings` (caught by Linux CI; mirrors the existing `kickstart` dual-cfg
/// pattern just above, generalised to also stay compiled for the test target
/// on every platform so `tests::apply_not_loaded_fallback_*` still run
/// cross-platform).
#[cfg(any(test, target_os = "macos"))]
fn attempt_verify_fallback(
    env: &dyn super::service_bootstrap::ServiceEnv,
    binary: &str,
) -> Option<bool> {
    if !env.plist_present(binary) {
        return None;
    }
    Some(env.bootstrap_fallback(binary).is_ok())
}

/// The complete #3841 second-layer repair: attempt the bootstrap fallback via
/// `env`, and on success, re-probe through the SAME bounded
/// [`poll_until_not_down`]/[`bounded_attempts`] machinery the #2498 kickstart
/// retry uses — never a single racy instant probe — before re-classifying.
///
/// Why: extracted as its own generic function (rather than inlined in
/// [`verify_one`]) so the WHOLE `NotLoaded -> fallback -> re-poll ->
/// re-classify` wiring is directly unit-testable against fakes, not just
/// [`attempt_verify_fallback`] in isolation (#3849 code-critic MEDIUM 2). Also
/// fixes #3849 code-critic MEDIUM 1: the pre-fix code re-probed exactly once,
/// instantly, after a successful fallback — racing a just-repaired but
/// slow-starting daemon (trusty-search's first-boot embedding-model download
/// notably) into a false `down`/`NOT VERIFIED`, the same race class #3833
/// already fixed for the kickstart path. `probe`/`wait`/`classify` are kept
/// generic (rather than calling `probe_member_health`/`std::thread::sleep`/
/// `classify_down_state` directly) so this composes hermetically in tests,
/// never sleeping or shelling out to `launchctl` in the test suite.
///
/// # Preconditions
/// Caller has already established the member is currently diagnosed
/// [`DownState::NotLoaded`] (`manage == ManageStrategy::Launchd`, health ==
/// `down`) — this function does not re-derive that starting condition.
/// # Postconditions
/// Returns `(health, down_state, verify_bootstrapped)`.
/// `verify_bootstrapped` is `true` iff [`attempt_verify_fallback`] found a
/// plist and attempted the fallback bootstrap, REGARDLESS of whether it
/// succeeded. When the fallback attempt did not run at all (no plist) or the
/// `bootstrap_fallback` call itself failed, `health`/`down_state` are
/// returned UNCHANGED (`health_str::DOWN` / `Some(DownState::NotLoaded)`) —
/// only a SUCCESSFUL fallback triggers the re-poll and re-classification.
/// When `remaining_budget` is already exhausted at the moment the fallback
/// succeeds, falls back to a single instant `probe()` (never an unbounded
/// wait) rather than skipping the re-probe entirely.
/// Test: `tests::apply_not_loaded_fallback_heals_after_poll_when_fallback_succeeds`,
/// `tests::apply_not_loaded_fallback_reports_still_not_loaded_when_fallback_fails`,
/// `tests::apply_not_loaded_fallback_noop_when_no_plist`,
/// `tests::apply_not_loaded_fallback_uses_instant_probe_when_budget_exhausted`.
///
/// `#[cfg(any(test, target_os = "macos"))]`: see [`attempt_verify_fallback`]'s
/// doc — same reasoning, same non-macOS-production dead-code hazard.
#[cfg(any(test, target_os = "macos"))]
fn apply_not_loaded_fallback<F, W, C>(
    env: &dyn super::service_bootstrap::ServiceEnv,
    binary: &str,
    manage: ManageStrategy,
    remaining_budget: Duration,
    mut probe: F,
    mut wait: W,
    classify: C,
) -> (String, Option<DownState>, bool)
where
    F: FnMut() -> String,
    W: FnMut(),
    C: Fn() -> DownState,
{
    let Some(succeeded) = attempt_verify_fallback(env, binary) else {
        return (
            health_str::DOWN.to_owned(),
            Some(DownState::NotLoaded),
            false,
        );
    };
    if !succeeded {
        return (
            health_str::DOWN.to_owned(),
            Some(DownState::NotLoaded),
            true,
        );
    }
    // #3849 MEDIUM 1: bounded re-poll, never a single instant re-probe — the
    // fallback just force-loaded the label, but the daemon behind it can
    // still take real wall-clock time to report healthy.
    let health = if remaining_budget.is_zero() {
        probe()
    } else {
        let (polled, _attempts) =
            poll_until_not_down(&mut probe, &mut wait, bounded_attempts(remaining_budget));
        polled
    };
    let down_state = if health == health_str::DOWN && manage == ManageStrategy::Launchd {
        Some(classify())
    } else {
        None
    };
    (health, down_state, true)
}

/// Real-wiring shell for [`apply_not_loaded_fallback`] — the only place that
/// names `RealServiceEnv`/`probe_member_health`/`classify_down_state`
/// (mirrors `service_bootstrap::bootstrap_member_service`'s split from
/// `bootstrap_one`).
///
/// Why: keeps [`verify_one`] itself free of platform `cfg` noise; on
/// non-macOS this is an unconditional no-op returning the SAME "untouched"
/// triple [`apply_not_loaded_fallback`] returns for its own no-plist case, so
/// callers never need to branch on platform.
/// What: on macOS, delegates to [`apply_not_loaded_fallback`] with
/// [`super::service_bootstrap::RealServiceEnv`] and the real probe/wait/
/// classify closures; on non-macOS, always `(down, Some(NotLoaded), false)`.
/// Test: side-effecting (real `launchctl` + sleep) on macOS; the decision
/// logic it wraps is `apply_not_loaded_fallback`'s own test suite.
#[cfg(target_os = "macos")]
fn apply_not_loaded_fallback_real(
    binary: &str,
    manage: ManageStrategy,
    remaining_budget: Duration,
) -> (String, Option<DownState>, bool) {
    apply_not_loaded_fallback(
        &super::service_bootstrap::RealServiceEnv,
        binary,
        manage,
        remaining_budget,
        || {
            probe_member_health(binary, manage)
                .health_string()
                .to_owned()
        },
        || std::thread::sleep(POLL_INTERVAL),
        || classify_down_state(binary),
    )
}

#[cfg(not(target_os = "macos"))]
fn apply_not_loaded_fallback_real(
    _binary: &str,
    _manage: ManageStrategy,
    _remaining_budget: Duration,
) -> (String, Option<DownState>, bool) {
    (
        health_str::DOWN.to_owned(),
        Some(DownState::NotLoaded),
        false,
    )
}

/// Verify one daemon member, applying the #2498 kickstart retry + #3833 poll
/// wait when needed (bounded by the #3836 aggregate poll budget), and the
/// #3841 second-layer bootstrap-fallback attempt for a `NotLoaded` diagnosis.
///
/// Why: isolates the per-member side effect (probe, maybe kickstart, poll,
/// classify, maybe fallback-bootstrap) so [`run_verify_tail`] stays a plain
/// fold over `selected`.
/// What: probes health; if [`needs_kickstart`]:
/// - and `remaining_budget` is already exhausted (#3836), skips the poll
///   entirely (only the instant probe already taken) and flags
///   `budget_exhausted: true` — no kickstart, no wait, no `launchctl` calls
///   for this member;
/// - otherwise, attempts one kickstart, prints a one-line "waiting for
///   services to start..." indication, then polls via [`poll_until_not_down`]
///   for [`bounded_attempts`]`(remaining_budget)` attempts (never more than
///   [`POLL_MAX_ATTEMPTS`], and fewer when the aggregate budget is running
///   low) instead of a single instant re-probe.
///
/// If the member is STILL `down` after all that, classifies WHY via
/// [`classify_down_state`] regardless of whether the budget was exhausted —
/// that check is a single cheap `launchctl list`, not a poll, so it's never
/// budget-gated. When the classification is [`DownState::NotLoaded`], calls
/// [`apply_not_loaded_fallback_real`] (#3841); on a successful fallback,
/// re-probes (via the SAME bounded poll machinery, #3849 MEDIUM 1) and
/// re-classifies so the returned row reflects the POST-repair state, not the
/// stale diagnosis. `remaining_budget` for that second poll is recomputed
/// from `start.elapsed()` rather than reusing the raw parameter, so it
/// correctly reflects whatever the kickstart poll immediately above already
/// spent within this SAME member's turn.
/// Test: side-effecting (subprocess + sleep); the decision halves are
/// `needs_kickstart`, `poll_until_not_down`, `bounded_attempts`,
/// `classify_down_state_from_entry`, `apply_not_loaded_fallback`.
fn verify_one(m: &StableMember, remaining_budget: Duration) -> VerifyRow {
    let start = Instant::now();
    let mut health = probe_member_health(&m.binary, m.manage)
        .health_string()
        .to_owned();
    let mut kickstarted = false;
    let mut budget_exhausted = false;
    if needs_kickstart(&health, m.manage) {
        if remaining_budget.is_zero() {
            budget_exhausted = true;
        } else {
            kickstarted = true;
            let _ = kickstart(&m.binary);
            eprintln!("  waiting for {} to start...", m.binary);
            let (polled_health, _attempts) = poll_until_not_down(
                || {
                    probe_member_health(&m.binary, m.manage)
                        .health_string()
                        .to_owned()
                },
                || std::thread::sleep(POLL_INTERVAL),
                bounded_attempts(remaining_budget),
            );
            health = polled_health;
        }
    }
    let mut down_state = if health == health_str::DOWN && m.manage == ManageStrategy::Launchd {
        Some(classify_down_state(&m.binary))
    } else {
        None
    };

    // #3841 belt-and-braces (layer 2): a `NotLoaded` diagnosis here means the
    // label was never bootstrapped by ANY earlier step this run took. Try
    // the installer's own fallback bootstrap one more time; on success,
    // `apply_not_loaded_fallback_real` re-polls (bounded by whatever of
    // `remaining_budget` is left after the kickstart phase above) rather than
    // a single racy instant probe (#3849 MEDIUM 1).
    let mut verify_bootstrapped = false;
    if matches!(down_state, Some(DownState::NotLoaded)) {
        let remaining_now = remaining_budget.saturating_sub(start.elapsed());
        let (new_health, new_down_state, attempted) =
            apply_not_loaded_fallback_real(&m.binary, m.manage, remaining_now);
        verify_bootstrapped = attempted;
        health = new_health;
        down_state = new_down_state;
    }

    VerifyRow {
        member: m.crate_name.clone(),
        health,
        kickstarted,
        required: m.required,
        budget_exhausted,
        down_state,
        verify_bootstrapped,
    }
}

/// Run the post-install verify tail over the selected members (#2560).
///
/// Why: THE entry point `install::run` calls (unless `--no-verify`) right
/// after `install_all` — the final "did this actually work" pass.
/// What: runs the `.mcp.json` patch + project-setup stages (reusing
/// `ensure`'s own logic, not its CLI render, so no duplicate JSON/stdout is
/// emitted), then verifies every DAEMON member in `selected` (with the #2498
/// kickstart retry), and builds the report.
/// Test: side-effecting (filesystem + network + subprocess); the pure pieces
/// it composes (`needs_kickstart`, `VerifyTailReport::build`) are unit tested.
pub fn run_verify_tail(selected: &[StableMember]) -> VerifyTailReport {
    use super::ensure::{mcp_patch, project_setup, report::EnsureReport};
    use super::runtime::block_on;

    let root = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let mcp_members = mcp_patch::patch_all();
    let stages = block_on(project_setup::run_stages(&root));
    // `wait`/`ready` are not evaluated here — `install`'s verify tail uses its
    // own targeted daemon health + kickstart retry instead of `ensure --wait`'s
    // generic readiness poll.
    let ensure_report = EnsureReport::build(mcp_members, stages, false, false);
    let ensure_ok = ensure_report.all_ok;

    // #3836 MEDIUM fix: `verify_one` folds sequentially over every daemon
    // member, and each one's poll wait can run up to ~55s — an AGGREGATE
    // budget (recomputed fresh from `budget_clock` before every member, so it
    // shrinks in real time as earlier members consume it) caps the total
    // time this loop can spend polling, so several simultaneously-stuck
    // daemons stall the installer by ~2 minutes at worst, not ~4-5.
    let budget_clock = Instant::now();
    let members: Vec<VerifyRow> = selected
        .iter()
        .filter(|m| m.daemon)
        .map(|m| {
            let remaining = AGGREGATE_POLL_BUDGET.saturating_sub(budget_clock.elapsed());
            verify_one(m, remaining)
        })
        .collect();

    VerifyTailReport::build(ensure_ok, members)
}

/// Whether stdout is a terminal (controls ANSI colour in [`print_human`]).
///
/// Why: a piped/CI invocation (e.g. inside `curl | sh`) must not emit raw
/// escape codes into a log file.
/// What: `true` iff stdout is a TTY.
/// Test: `tests::use_color_returns_bool`.
fn use_color() -> bool {
    std::io::IsTerminal::is_terminal(&std::io::stdout())
}

/// Render `label` in green (colour) or plain text.
fn colour(label: &str, ansi: &str) -> String {
    if use_color() {
        format!("\x1b[{ansi}m{label}\x1b[0m")
    } else {
        label.to_owned()
    }
}

/// The per-row annotation `print_human` appends after the health string.
///
/// Why: Extracted as a pure function (rather than inline in `print_human`) so
/// the demo-relevant wording can be unit-tested without capturing stdout —
/// mirrors the pattern used throughout this crate (e.g. `install.rs`'s
/// `select_prebuilt_bin_path`) for exactly this reason. #3797 critic finding
/// (MEDIUM, demo-relevant): an OPTIONAL daemon that installed but whose
/// launchd bootstrap failed reports health `down`, which previously got NO
/// qualifier here — a bare `trusty-console  down` printed immediately above
/// the green `VERIFIED` line reads as alarming to a demo viewer even though
/// [`VerifyTailReport::build`] already correctly excludes it from the
/// verdict/exit code. Widening the condition to cover `down` (not just
/// `not_installed`) makes the annotation match the verdict's own tolerance.
/// #3833: a member still `down` after the poll wait now carries a
/// [`DownState`] diagnosis — that always wins (it is a DIAGNOSIS, not a
/// reassurance, so it is shown for REQUIRED members too, just without the
/// "optional" framing). #3841: `verify_bootstrapped` adds its own note —
/// `", fallback attempted"` appended to a diagnosis that is STILL present
/// (the second-layer repair also failed or the label remained unloaded), or
/// `" (bootstrapped by installer)"` on its own when the fallback resolved the
/// problem (`down_state` is `None` again).
/// What: `" (<down-state phrase>[, fallback attempted])"` (optionally
/// `"optional, "`-prefixed) when `down_state` is `Some`;
/// `" (bootstrapped by installer)"` when `verify_bootstrapped` resolved the
/// member; `" (kickstarted)"` when a retry fired and the member came back
/// healthy; `" (optional, skipped)"` when the member is OPTIONAL and
/// `not_installed`; otherwise empty.
/// Test: `tests::optional_annotation_covers_not_installed_and_down`,
/// `tests::optional_annotation_reports_down_state`,
/// `tests::optional_annotation_reports_verify_bootstrapped`.
fn optional_annotation(m: &VerifyRow) -> String {
    if let Some(reason) = &m.down_state {
        let phrase = reason.phrase();
        let phrase = if m.verify_bootstrapped {
            format!("{phrase}, fallback attempted")
        } else {
            phrase
        };
        return if m.required {
            format!(" ({phrase})")
        } else {
            format!(" (optional, {phrase})")
        };
    }
    if m.verify_bootstrapped {
        return " (bootstrapped by installer)".to_owned();
    }
    if m.kickstarted {
        " (kickstarted)".to_owned()
    } else if !m.required && m.health == health_str::NOT_INSTALLED {
        " (optional, skipped)".to_owned()
    } else {
        String::new()
    }
}

/// Whether any member in `members` had its poll wait skipped because the
/// #3836 aggregate poll budget was already exhausted.
///
/// Why: extracted as a pure, unit-testable predicate so [`print_human`]'s
/// "budget exhausted" summary note is driven by a tested decision, not
/// inline logic only exercisable via stdout capture.
/// What: `true` iff any row has `budget_exhausted: true`.
/// Test: `tests::any_budget_exhausted_*`.
fn any_budget_exhausted(members: &[VerifyRow]) -> bool {
    members.iter().any(|m| m.budget_exhausted)
}

/// Print the final human-readable verify-tail summary — green/red pass/fail.
///
/// Why: the last thing the `curl | sh` one-liner (via `install.sh` ->
/// `tctl install`) prints must be an unambiguous verified/not-verified signal.
/// What: prints the ensure verdict, one line per daemon member (noting a
/// kickstart retry, a #3833 `down_state` diagnosis, or an optional-and-
/// tolerated bad health via [`optional_annotation`]); when
/// [`any_budget_exhausted`] (#3836), one summary note (not repeated per row)
/// pointing the operator at re-running `tctl install`; and a final
/// colour-coded `VERIFIED`/`NOT VERIFIED` line.
/// Test: side-effect-only (stdout); the annotation text is unit-tested via
/// `optional_annotation`/`any_budget_exhausted` and the data they read via
/// `VerifyTailReport::build`.
pub fn print_human(report: &VerifyTailReport) {
    println!("tctl install — verify");
    println!(
        "  ensure               {}",
        if report.ensure_ok { "ok" } else { "FAILED" }
    );
    for m in &report.members {
        println!("  {:<20} {}{}", m.member, m.health, optional_annotation(m));
    }
    if any_budget_exhausted(&report.members) {
        println!(
            "  note: poll budget exhausted — giving up on remaining members; \
             re-run `tctl install` to check them again"
        );
    }
    let verdict = if report.verified {
        colour("VERIFIED", "32")
    } else {
        colour("NOT VERIFIED", "31")
    };
    println!("verify: {verdict}");
}

#[cfg(test)]
#[path = "verify_tail_tests.rs"]
mod tests;
