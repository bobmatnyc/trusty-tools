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
/// for this member; `required` (graceful-degrade, demo-critical fix) mirrors
/// `StableMember::required` and gates whether a down/not-installed verdict for
/// this member may fail the overall `verified` verdict.
/// Test: `tests::report_serialises`.
#[derive(Clone, Debug, Serialize)]
pub struct VerifyRow {
    /// Crate name.
    pub member: String,
    /// Health verdict after any kickstart retry.
    pub health: String,
    /// Whether a `launchctl kickstart -k` retry was attempted (#2498).
    pub kickstarted: bool,
    /// Whether this member is REQUIRED for the overall `verified` verdict.
    pub required: bool,
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
        required: m.required,
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
/// What: `" (kickstarted)"` when a retry fired; `" (optional, skipped)"` when
/// the member is OPTIONAL and its health is `not_installed` OR `down`;
/// otherwise empty.
/// Test: `tests::optional_annotation_covers_not_installed_and_down`.
fn optional_annotation(m: &VerifyRow) -> &'static str {
    if m.kickstarted {
        " (kickstarted)"
    } else if !m.required && (m.health == health_str::NOT_INSTALLED || m.health == health_str::DOWN)
    {
        " (optional, skipped)"
    } else {
        ""
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
    /// that installed but whose service bootstrap failed reports health
    /// `down`; that must read as "(optional, skipped)" just like
    /// `not_installed` does, not as a bare, alarming `down` right above the
    /// green `VERIFIED` line. A REQUIRED member's `down`/`not_installed` must
    /// get NO such softening annotation, and `kickstarted` must win over both.
    /// What: exercises the full truth table.
    /// Test: This is the test.
    #[test]
    fn optional_annotation_covers_not_installed_and_down() {
        assert_eq!(
            optional_annotation(&optional_row("trusty-console", health_str::NOT_INSTALLED)),
            " (optional, skipped)"
        );
        assert_eq!(
            optional_annotation(&optional_row("trusty-console", health_str::DOWN)),
            " (optional, skipped)"
        );
        assert_eq!(
            optional_annotation(&optional_row("trusty-console", health_str::HEALTHY)),
            "",
            "a healthy optional member needs no annotation"
        );
        assert_eq!(
            optional_annotation(&row("trusty-search", health_str::DOWN)),
            "",
            "a REQUIRED member's down health must not be softened"
        );
        assert_eq!(
            optional_annotation(&row("trusty-search", health_str::NOT_INSTALLED)),
            "",
            "a REQUIRED member's not_installed health must not be softened"
        );
        let kickstarted = VerifyRow {
            kickstarted: true,
            ..optional_row("trusty-console", health_str::DOWN)
        };
        assert_eq!(
            optional_annotation(&kickstarted),
            " (kickstarted)",
            "a kickstart retry note takes priority over the optional softening"
        );
    }
}
