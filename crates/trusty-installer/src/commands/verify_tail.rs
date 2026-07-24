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
//!    distinct end states via [`classify_down_state`] — [`DownState::NotLoaded`],
//!    [`DownState::Crashed`], or [`DownState::StillStarting`] — instead of a
//!    uniform, underspecified `down` (#3833).
//!
//! [`print_human`] renders the final green/red pass/fail summary — the last
//! thing the installer prints. Because the health used to build the report is
//! already the POST-wait value, the exit code CI/automation gates on
//! ([`super::install::InstallReport`], folded from `verified`) reflects the
//! final state, not the instant-after-kickstart snapshot.
//!
//! Test: the pure decision pieces (`needs_kickstart`, `poll_until_not_down`,
//! `classify_down_state_from_entry`, the report's `verified` derivation) are
//! unit-tested; the `ensure` phase and the `launchctl` subprocess calls are
//! side-effecting and validated manually.

use std::time::Duration;

use serde::Serialize;

use super::probe::{health_str, probe_member_health};
use super::stable_set::{ManageStrategy, StableMember};

/// Total wall-clock budget [`poll_until_not_down`] spends waiting for a
/// kickstarted daemon to report healthy (#3833).
///
/// Why: trusty-search downloads embedding models on first boot and can take
/// tens of seconds; a single instant re-probe (the pre-#3833 behaviour) raced
/// that window. ~60s covers a cold-start model download on a normal
/// connection without holding up `tctl install` indefinitely on a genuinely
/// dead daemon.
const POLL_INTERVAL: Duration = Duration::from_secs(5);

/// Number of probe attempts [`poll_until_not_down`] makes (one immediately,
/// then one every [`POLL_INTERVAL`]) — `12` attempts spread over 11 intervals
/// is ~55s of total wait, within the ~30-60s budget.
const POLL_MAX_ATTEMPTS: u32 = 12;

/// One member's verify-tail outcome.
///
/// Why: a typed row keeps the `--json` output stable + testable.
/// What: `member` crate name; `health` the (possibly kickstart-retried, POST
/// wait) health verdict string; `kickstarted` whether a #2498 kickstart retry
/// was attempted for this member; `required` (graceful-degrade, demo-critical
/// fix) mirrors `StableMember::required` and gates whether a down/not-installed
/// verdict for this member may fail the overall `verified` verdict;
/// `down_state` (#3833) is `Some` iff `health` is still `down` for a LAUNCHD
/// member after the wait, classifying WHY.
/// Test: `tests::report_serialises`.
#[derive(Clone, Debug, Serialize)]
pub struct VerifyRow {
    /// Crate name.
    pub member: String,
    /// Health verdict after any kickstart retry + poll wait.
    pub health: String,
    /// Whether a `launchctl kickstart -k` retry was attempted (#2498).
    pub kickstarted: bool,
    /// Whether this member is REQUIRED for the overall `verified` verdict.
    pub required: bool,
    /// Why a LAUNCHD member is still `down` after the poll wait, when it is
    /// (#3833). `None` for a healthy/stale/unknown/not_installed member, or a
    /// non-LAUNCHD member.
    pub down_state: Option<DownState>,
}

/// Why a LAUNCHD daemon member is still reporting `down` after the #3833 poll
/// wait — replaces a uniform, underspecified `down` with a diagnosis an
/// operator can act on directly.
///
/// Why: "down" alone doesn't tell you whether launchd never loaded the job at
/// all, loaded it and it crashed, or it is simply still coming up — three
/// situations with three different next actions (re-run the bootstrap step,
/// read the crash log, or just wait longer).
/// What: [`DownState::NotLoaded`] — `launchctl list <label>` found no such
/// service (never bootstrapped, or booted out). [`DownState::Crashed`] —
/// loaded, no PID currently running, and the last recorded exit status was
/// nonzero. [`DownState::StillStarting`] — loaded, and either a PID is
/// currently running (health just hasn't caught up yet) or the last exit
/// status was a clean `0` (about to be (re)launched, e.g. between
/// `ThrottleInterval` respawns).
/// Test: `tests::classify_down_state_from_entry_*`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum DownState {
    /// `launchctl` has no record of this label at all.
    NotLoaded,
    /// Loaded, but not currently running, with a nonzero last exit status.
    Crashed {
        /// The `LastExitStatus` launchd recorded.
        exit_code: i32,
    },
    /// Loaded and either currently running (health hasn't caught up) or
    /// cleanly exited and awaiting its next (re)launch.
    StillStarting,
}

impl DownState {
    /// A short, human-readable phrase for [`optional_annotation`]/`print_human`.
    ///
    /// Why: centralises the wording so the JSON `state` tag and the human
    /// narration can never drift.
    /// What: maps each variant to a lowercase phrase with no leading article.
    /// Test: `tests::down_state_phrase_mapping`.
    fn phrase(&self) -> String {
        match self {
            DownState::NotLoaded => "not loaded".to_owned(),
            DownState::Crashed { exit_code } => format!("crashed, exit {exit_code}"),
            DownState::StillStarting => "still starting".to_owned(),
        }
    }
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

/// One `launchctl list <label>` observation, pre-parsed (#3833).
///
/// Why: separating the parse from the subprocess spawn lets
/// [`classify_down_state_from_entry`] be exercised with fixed sample text —
/// no live `launchctl` needed.
/// What: `has_pid` — whether the dump has a `"PID" = N;` line (a process is
/// currently running); `exit_status` — the `"LastExitStatus"` launchd last
/// recorded (`0` when absent, matching launchd's own default).
/// Test: `tests::parse_launchd_list_text_*`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LaunchdListEntry {
    has_pid: bool,
    exit_status: i32,
}

/// Parse `launchctl list <label>`'s property-list-style stdout dump.
///
/// Why: isolates the (pure) text parsing from the (side-effecting) subprocess
/// spawn so the format assumptions are unit-testable.
/// What: scans for a `"PID" = ...;` line (sets `has_pid`) and a
/// `"LastExitStatus" = N;` line (sets `exit_status`, defaulting to `0` when
/// absent — launchd omits the key before the job has ever exited). Never
/// panics on malformed input.
/// Test: `tests::parse_launchd_list_text_running`,
/// `tests::parse_launchd_list_text_crashed`,
/// `tests::parse_launchd_list_text_clean_exit`.
fn parse_launchd_list_text(text: &str) -> LaunchdListEntry {
    let has_pid = text.lines().any(|l| l.trim_start().starts_with("\"PID\""));
    let exit_status = text
        .lines()
        .find_map(|l| l.trim().strip_prefix("\"LastExitStatus\" ="))
        .and_then(|rest| rest.trim().trim_end_matches(';').trim().parse::<i32>().ok())
        .unwrap_or(0);
    LaunchdListEntry {
        has_pid,
        exit_status,
    }
}

/// Classify a parsed `launchctl list` observation into a [`DownState`].
///
/// Why: the pure decision half of [`classify_down_state`] — kept separate so
/// every branch is unit-tested without a live `launchctl`.
/// What: `None` (label not found) → [`DownState::NotLoaded`]; a running PID →
/// [`DownState::StillStarting`] (loaded, health just hasn't caught up); a
/// nonzero last exit status with no running PID → [`DownState::Crashed`];
/// anything else (loaded, no PID, clean last exit) → [`DownState::StillStarting`]
/// (about to be (re)launched).
/// Test: `tests::classify_down_state_from_entry_not_loaded`,
/// `tests::classify_down_state_from_entry_running`,
/// `tests::classify_down_state_from_entry_crashed`,
/// `tests::classify_down_state_from_entry_clean_exit_no_pid`.
fn classify_down_state_from_entry(entry: Option<LaunchdListEntry>) -> DownState {
    match entry {
        None => DownState::NotLoaded,
        Some(e) if e.has_pid => DownState::StillStarting,
        Some(e) if e.exit_status != 0 => DownState::Crashed {
            exit_code: e.exit_status,
        },
        Some(_) => DownState::StillStarting,
    }
}

/// Run `launchctl list <label>` and return its raw stdout, or `None` when the
/// label is not found (macOS only; #3833).
///
/// Why: isolated as its own thin side-effecting function so
/// [`classify_down_state`] composes it with the pure
/// [`parse_launchd_list_text`] / [`classify_down_state_from_entry`] pair.
/// What: `Some(stdout)` on a successful `launchctl list <label>`; `None` when
/// the command fails to spawn or exits non-zero (launchd's "no such service"
/// signature).
/// Test: side-effecting; not invoked in the test suite.
#[cfg(target_os = "macos")]
fn launchd_list_raw(label: &str) -> Option<String> {
    let out = std::process::Command::new("launchctl")
        .args(["list", label])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(not(target_os = "macos"))]
fn launchd_list_raw(_label: &str) -> Option<String> {
    None
}

/// Classify why a LAUNCHD member is still `down` after the poll wait (#3833).
///
/// Why: the single entry point [`verify_one`] calls once it has a final
/// `down` verdict for a LAUNCHD member — composes the side-effecting
/// `launchctl list` call with the pure parse + classify pair above.
/// What: resolves the member's launchd label, runs `launchctl list <label>`,
/// and classifies the result via [`classify_down_state_from_entry`].
/// Test: side-effecting (subprocess); the decision half is
/// `classify_down_state_from_entry`.
fn classify_down_state(binary: &str) -> DownState {
    let label = super::plist_label::plist_label_for(binary);
    let entry = launchd_list_raw(&label).map(|text| parse_launchd_list_text(&text));
    classify_down_state_from_entry(entry)
}

/// Verify one daemon member, applying the #2498 kickstart retry + #3833 poll
/// wait when needed.
///
/// Why: isolates the per-member side effect (probe, maybe kickstart, poll,
/// classify) so [`run_verify_tail`] stays a plain fold over `selected`.
/// What: probes health; if [`needs_kickstart`], attempts one kickstart,
/// prints a one-line "waiting for services to start..." indication, then
/// polls via [`poll_until_not_down`] on the [`POLL_INTERVAL`] /
/// [`POLL_MAX_ATTEMPTS`] schedule (~60s total) instead of a single instant
/// re-probe. If the member is STILL `down` after that wait, classifies WHY
/// via [`classify_down_state`].
/// Test: side-effecting (subprocess + sleep); the decision halves are
/// `needs_kickstart`, `poll_until_not_down`, `classify_down_state_from_entry`.
fn verify_one(m: &StableMember) -> VerifyRow {
    let mut health = probe_member_health(&m.binary, m.manage);
    let mut kickstarted = false;
    if needs_kickstart(&health, m.manage) {
        kickstarted = true;
        let _ = kickstart(&m.binary);
        eprintln!("  waiting for {} to start...", m.binary);
        let (polled_health, _attempts) = poll_until_not_down(
            || probe_member_health(&m.binary, m.manage),
            || std::thread::sleep(POLL_INTERVAL),
            POLL_MAX_ATTEMPTS,
        );
        health = polled_health;
    }
    let down_state = if health == health_str::DOWN && m.manage == ManageStrategy::Launchd {
        Some(classify_down_state(&m.binary))
    } else {
        None
    };
    VerifyRow {
        member: m.crate_name.clone(),
        health,
        kickstarted,
        required: m.required,
        down_state,
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

    let members: Vec<VerifyRow> = selected
        .iter()
        .filter(|m| m.daemon)
        .map(verify_one)
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
/// "optional" framing).
/// What: `" (<down-state phrase>)"` (optionally `"optional, "`-prefixed) when
/// `down_state` is `Some`; `" (kickstarted)"` when a retry fired and the
/// member came back healthy; `" (optional, skipped)"` when the member is
/// OPTIONAL and `not_installed`; otherwise empty.
/// Test: `tests::optional_annotation_covers_not_installed_and_down`,
/// `tests::optional_annotation_reports_down_state`.
fn optional_annotation(m: &VerifyRow) -> String {
    if let Some(reason) = &m.down_state {
        let phrase = reason.phrase();
        return if m.required {
            format!(" ({phrase})")
        } else {
            format!(" (optional, {phrase})")
        };
    }
    if m.kickstarted {
        " (kickstarted)".to_owned()
    } else if !m.required && m.health == health_str::NOT_INSTALLED {
        " (optional, skipped)".to_owned()
    } else {
        String::new()
    }
}

/// Print the final human-readable verify-tail summary — green/red pass/fail.
///
/// Why: the last thing the `curl | sh` one-liner (via `install.sh` ->
/// `tctl install`) prints must be an unambiguous verified/not-verified signal.
/// What: prints the ensure verdict, one line per daemon member (noting a
/// kickstart retry or an optional-and-tolerated bad health via
/// [`optional_annotation`]), and a final colour-coded `VERIFIED`/`NOT
/// VERIFIED` line.
/// Test: side-effect-only (stdout); the annotation text is unit-tested via
/// `optional_annotation` and the data it reads via `VerifyTailReport::build`.
pub fn print_human(report: &VerifyTailReport) {
    println!("tctl install — verify");
    println!(
        "  ensure               {}",
        if report.ensure_ok { "ok" } else { "FAILED" }
    );
    for m in &report.members {
        println!("  {:<20} {}{}", m.member, m.health, optional_annotation(m));
    }
    let verdict = if report.verified {
        colour("VERIFIED", "32")
    } else {
        colour("NOT VERIFIED", "31")
    };
    println!("verify: {verdict}");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(member: &str, health: &str) -> VerifyRow {
        VerifyRow {
            member: member.to_owned(),
            health: health.to_owned(),
            kickstarted: false,
            required: true,
            down_state: None,
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

    /// Why: pins the exact wording each `DownState` variant renders, since
    /// both the human summary and (transitively, via `serde`) the `--json`
    /// `state` tag depend on it staying stable.
    /// What: asserts the phrase for each variant.
    /// Test: This is the test.
    #[test]
    fn down_state_phrase_mapping() {
        assert_eq!(DownState::NotLoaded.phrase(), "not loaded");
        assert_eq!(
            DownState::Crashed { exit_code: 78 }.phrase(),
            "crashed, exit 78"
        );
        assert_eq!(DownState::StillStarting.phrase(), "still starting");
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

    /// Why: the running-PID branch must win regardless of last exit status —
    /// a currently-running process means the daemon IS up; `down` health
    /// just hasn't caught up yet (e.g. still loading models).
    /// What: an entry with `has_pid: true` classifies as `StillStarting`
    /// even when `exit_status` is nonzero (stale from a PRIOR crash).
    /// Test: This is the test.
    #[test]
    fn classify_down_state_from_entry_running() {
        let entry = LaunchdListEntry {
            has_pid: true,
            exit_status: 1,
        };
        assert_eq!(
            classify_down_state_from_entry(Some(entry)),
            DownState::StillStarting
        );
    }

    /// Why: THE #3833 core diagnosis — no running PID plus a nonzero last
    /// exit status is a genuine crash, not a startup race.
    /// What: asserts `Crashed` carries the exact exit code.
    /// Test: This is the test.
    #[test]
    fn classify_down_state_from_entry_crashed() {
        let entry = LaunchdListEntry {
            has_pid: false,
            exit_status: 2,
        };
        assert_eq!(
            classify_down_state_from_entry(Some(entry)),
            DownState::Crashed { exit_code: 2 }
        );
    }

    /// Why: no PID + a clean (`0`) last exit is ambiguous between "never
    /// started yet" and "cleanly stopped, about to relaunch" — both read as
    /// "still starting" rather than a false `Crashed`.
    /// What: asserts `StillStarting`.
    /// Test: This is the test.
    #[test]
    fn classify_down_state_from_entry_clean_exit_no_pid() {
        let entry = LaunchdListEntry {
            has_pid: false,
            exit_status: 0,
        };
        assert_eq!(
            classify_down_state_from_entry(Some(entry)),
            DownState::StillStarting
        );
    }

    /// Why: THE #3832/#3833 root-cause signature — a label `launchctl list`
    /// cannot find at all (never bootstrapped, e.g. the #3832 trusty-memory
    /// bug) must be reported as `NotLoaded`, not lumped in with a crash.
    /// What: asserts `None` (label not found) classifies as `NotLoaded`.
    /// Test: This is the test.
    #[test]
    fn classify_down_state_from_entry_not_loaded() {
        assert_eq!(classify_down_state_from_entry(None), DownState::NotLoaded);
    }

    /// Why: `launchctl list <label>`'s dump format is `"Key" = value;` lines
    /// inside a `{ ... }` block; the parser must find `PID`/`LastExitStatus`
    /// regardless of surrounding whitespace/indentation and must not mistake
    /// unrelated keys (e.g. `"PerJobMachServices"`) for them.
    /// What: parses a realistic "running" dump; asserts `has_pid` and the
    /// default `exit_status` (absent key → `0`).
    /// Test: This is the test.
    #[test]
    fn parse_launchd_list_text_running() {
        let text = r#"{
	"LimitLoadToSessionType" = "Aqua";
	"Label" = "com.trusty.trusty-search";
	"OnDemand" = false;
	"LastExitStatus" = 0;
	"PID" = 4242;
	"Program" = "/usr/local/bin/trusty-search";
};
"#;
        let entry = parse_launchd_list_text(text);
        assert!(entry.has_pid);
        assert_eq!(entry.exit_status, 0);
    }

    /// Why: the crash signature — no `PID` line, a nonzero `LastExitStatus`.
    /// What: parses a realistic "crashed" dump; asserts `has_pid` is false
    /// and `exit_status` matches.
    /// Test: This is the test.
    #[test]
    fn parse_launchd_list_text_crashed() {
        let text = r#"{
	"LimitLoadToSessionType" = "Aqua";
	"Label" = "com.trusty.memory";
	"LastExitStatus" = 78;
};
"#;
        let entry = parse_launchd_list_text(text);
        assert!(!entry.has_pid);
        assert_eq!(entry.exit_status, 78);
    }

    /// Why: a loaded-but-never-yet-run (or cleanly stopped) job omits `PID`
    /// and reports `LastExitStatus = 0` (or omits it entirely) — must not be
    /// misparsed as a crash.
    /// What: parses a dump with neither key; asserts `has_pid: false`,
    /// `exit_status: 0` (the documented default).
    /// Test: This is the test.
    #[test]
    fn parse_launchd_list_text_clean_exit() {
        let text = r#"{
	"Label" = "com.trusty.trusty-review";
	"OnDemand" = false;
};
"#;
        let entry = parse_launchd_list_text(text);
        assert!(!entry.has_pid);
        assert_eq!(entry.exit_status, 0);
    }
}
