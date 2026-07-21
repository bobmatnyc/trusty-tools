//! Post-install verify tail: `ensure` + health, with the #2498 kickstart retry
//! (#2560).
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
//!    `launchctl kickstart -k gui/<uid>/<label>` and re-probes once before
//!    accepting `down` as the final verdict for that member.
//!
//! [`print_human`] renders the final green/red pass/fail summary — the last
//! thing the installer prints.
//!
//! Test: the pure decision (`needs_kickstart`) and the report's `verified`
//! derivation are unit-tested; the `ensure` phase and `launchctl kickstart` are
//! side-effecting and validated manually.

use serde::Serialize;

use super::probe::{health_str, probe_member_health};
use super::stable_set::{ManageStrategy, StableMember};

/// One member's verify-tail outcome.
///
/// Why: a typed row keeps the `--json` output stable + testable.
/// What: `member` crate name; `health` the (possibly kickstart-retried) health
/// verdict string; `kickstarted` whether a #2498 kickstart retry was attempted
/// for this member.
/// Test: `tests::report_serialises`.
#[derive(Clone, Debug, Serialize)]
pub struct VerifyRow {
    /// Crate name.
    pub member: String,
    /// Health verdict after any kickstart retry.
    pub health: String,
    /// Whether a `launchctl kickstart -k` retry was attempted (#2498).
    pub kickstarted: bool,
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
    /// Overall verdict: `ensure_ok` AND every member healthy/stale/unknown
    /// (never `down`/`not_installed`).
    pub verified: bool,
}

impl VerifyTailReport {
    /// Build a report and derive the overall `verified` verdict.
    ///
    /// Why: one place derives the verdict so the JSON and the folded
    /// `InstallReport.all_ok` can never disagree.
    /// What: `verified = ensure_ok AND no member is `down`/`not_installed``. A
    /// `stale` or `unknown` member does NOT fail verification — mirrors
    /// `stack::health::HealthReport`'s degrade policy exactly (an
    /// under-the-version-floor daemon is still up; an unprobeable
    /// process-managed member is not a verified gap).
    /// Test: `tests::verified_requires_ensure_and_health`,
    /// `tests::verified_tolerates_stale_and_unknown`.
    fn build(ensure_ok: bool, members: Vec<VerifyRow>) -> Self {
        let health_ok = members
            .iter()
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
/// it exited successfully. On non-macOS, always returns `false` (no-op).
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

/// Verify one daemon member, applying the #2498 kickstart retry when needed.
///
/// Why: isolates the per-member side effect (probe, maybe kickstart, re-probe)
/// so [`run_verify_tail`] stays a plain fold over `selected`.
/// What: probes health; if [`needs_kickstart`], attempts one kickstart and
/// re-probes; returns the (possibly updated) health plus whether a kickstart
/// was attempted.
/// Test: side-effecting (subprocess); the decision half is `needs_kickstart`.
fn verify_one(m: &StableMember) -> VerifyRow {
    let mut health = probe_member_health(&m.binary, m.manage);
    let mut kickstarted = false;
    if needs_kickstart(&health, m.manage) {
        kickstarted = kickstart(&m.binary);
        if kickstarted {
            health = probe_member_health(&m.binary, m.manage);
        }
    }
    VerifyRow {
        member: m.crate_name.clone(),
        health,
        kickstarted,
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

/// Print the final human-readable verify-tail summary — green/red pass/fail.
///
/// Why: the last thing the `curl | sh` one-liner (via `install.sh` ->
/// `tctl install`) prints must be an unambiguous verified/not-verified signal.
/// What: prints the ensure verdict, one line per daemon member (noting a
/// kickstart retry), and a final colour-coded `VERIFIED`/`NOT VERIFIED` line.
/// Test: side-effect-only (stdout); the data it reads is unit-tested via
/// `VerifyTailReport::build`.
pub fn print_human(report: &VerifyTailReport) {
    println!("tctl install — verify");
    println!(
        "  ensure               {}",
        if report.ensure_ok { "ok" } else { "FAILED" }
    );
    for m in &report.members {
        let note = if m.kickstarted { " (kickstarted)" } else { "" };
        println!("  {:<20} {}{}", m.member, m.health, note);
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

    /// Why: `print_human`'s colour path must never panic regardless of the
    /// harness's stdout fd; this just confirms it is callable.
    /// What: calls `use_color`, binds the result.
    /// Test: This is the test.
    #[test]
    fn use_color_returns_bool() {
        let _v: bool = use_color();
    }
}
