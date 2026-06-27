//! The staged boot sequence (DOC-12 §3 STAGE 0–5).
//!
//! Why: This is the heart of `tctl up` — the dependency-ordered, readiness-gated
//! sequence the owner directive (Bob, 2026-06-15) requires. Keeping it in its
//! own module separates the *sequencing* (here) from the *primitives*
//! (member.rs, claude_code.rs, report.rs) and from the *entry/wiring* (mod.rs).
//!
//! What: `run_sequence` drives STAGE 1 (always-on core, gated) → STAGE 2 (boot
//! analysis, background/non-gating) → STAGE 3 (console, starts on partial core)
//! → STAGE 4 (opt-in orchestrator + STAGE 4b Claude Code) → STAGE 5
//! (ensure-project), accumulating an `UpReport`. STAGE 0 (lock + config) is the
//! caller's (mod.rs) responsibility so the lock lifetime spans the sequence.
//!
//! Test: `super::tests::sequence_*` drive `run_sequence` against mock runners /
//! claude envs across the healthy / core-down / opt-in matrices.

use super::claude_code::{ensure_claude_code, ClaudeEnv};
use super::config::AnalyzeMode;
use super::manifest::{members_in_stage, BootMember, BootStage};
use super::member::{ensure_member, MemberHealth, Runner};
use super::policy::{OrchestratorDecision, ResolvedBootPolicy};
use super::report::{StageReport, StageStatus, UpReport};

/// Console URL probe seam (DOC-12 §3 STAGE 3).
///
/// Why: After starting the console, `tctl up` prints its URL (the user's web
/// entry). The URL is discovered via the console's `port --json`; hiding that
/// behind a trait keeps the sequence testable.
///
/// What: One method returning the console URL when discoverable.
///
/// Test: `super::tests` supplies a stub returning a fixed URL.
pub trait ConsoleLocator {
    /// Discover the running console's URL, if any.
    fn console_url(&self) -> Option<String>;
}

/// Everything STAGE 4 needs to ensure the opt-in orchestrator's runtime.
///
/// Why: STAGE 4b (Claude Code) needs the runtime metadata + a `ClaudeEnv`;
/// bundling them keeps `run_sequence`'s signature readable.
///
/// What: The `ClaudeEnv` seam plus the resolved policy fields STAGE 4b reads.
///
/// Test: Constructed in `super::tests`.
pub struct OrchestratorDeps<'a> {
    /// Injectable Claude Code environment (§6).
    pub claude: &'a dyn ClaudeEnv,
}

/// Drive STAGE 1–5 of the boot sequence, returning the accumulated report.
///
/// Why: Single function encoding the §3 ordering and the §5 gating so the order
/// is read in one place and is unit-testable end to end.
///
/// What: See the stage breakdown below. The §5 hard rule is enforced: if the
/// core stage fails, STAGE 4 (orchestrator) is skipped entirely, but STAGE 3
/// (console) still runs on partial core for observability (RESOLVED Q2).
///
/// Test: `super::tests::sequence_clean_all_ok`,
/// `sequence_core_down_skips_orchestrator`,
/// `sequence_console_starts_on_partial_core`,
/// `sequence_optin_off_skips_orchestrator`.
pub fn run_sequence(
    members: &[BootMember],
    policy: &ResolvedBootPolicy,
    runner: &dyn Runner,
    console: &dyn ConsoleLocator,
    orch: &OrchestratorDeps<'_>,
) -> UpReport {
    let mut report = UpReport::default();

    // ── STAGE 1 — ALWAYS-ON CORE (gated) ────────────────────────────────────
    let (core_status, core_detail, any_core_healthy) = stage_core(members, runner);
    report.push(StageReport::new("core", core_status, core_detail));
    let core_ok = core_status == StageStatus::Ok;

    // ── STAGE 2 — BOOT ANALYSIS (background, non-gating) ─────────────────────
    report.push(stage_analyze(members, runner, policy));

    // ── STAGE 3 — CONSOLE (starts even on partial core for observability) ────
    let console_report = stage_console(members, runner, console, any_core_healthy, &mut report);
    let _ = console_report;

    // ── STAGE 4 — OPT-IN ORCHESTRATOR (skipped on any core failure) ──────────
    if !core_ok {
        report.push(StageReport::new(
            "orchestrator",
            StageStatus::Skipped,
            "skipped — core stage did not pass (§5 hard stop)",
        ));
    } else {
        stage_orchestrator(members, policy, runner, orch, &mut report);
    }

    // ── STAGE 5 — ENSURE-PROJECT (only meaningful over a healthy core) ───────
    if core_ok {
        let mode = if policy.wait {
            "blocking"
        } else {
            "background reindex"
        };
        report.push(StageReport::new(
            "ensure-project",
            StageStatus::Ok,
            format!("project auto-config ({mode})"),
        ));
    } else {
        report.push(StageReport::new(
            "ensure-project",
            StageStatus::Skipped,
            "skipped — core stage did not pass",
        ));
    }

    report
}

/// STAGE 1 — ensure every always-on core member, AND-gating on all healthy.
///
/// Why: Nothing downstream is meaningful over a dead core (§3.1); the stage gate
/// is the AND of both core members reaching `healthy + version-ok` (§3.2).
///
/// What: Ensures each `boot_stage = core` member (CHECK→act→VERIFY). Returns the
/// stage status (`Ok` iff all gated-ok, else `Failed`), a detail summary, and
/// whether *any* core member is healthy (drives the §5 partial-core console).
///
/// Test: `super::tests::sequence_clean_all_ok`,
/// `sequence_core_down_skips_orchestrator`.
fn stage_core(members: &[BootMember], runner: &dyn Runner) -> (StageStatus, String, bool) {
    let core = members_in_stage(members, BootStage::Core);
    let mut all_ok = true;
    let mut any_healthy = false;
    let mut parts = Vec::new();
    for m in core {
        let outcome = ensure_member(runner, m);
        if outcome.gated_ok {
            any_healthy = true;
        } else {
            all_ok = false;
        }
        parts.push(format!("{}={}", outcome.member, outcome.action));
    }
    let status = if all_ok && !parts.is_empty() {
        StageStatus::Ok
    } else {
        StageStatus::Failed
    };
    (status, parts.join(", "), any_healthy)
}

/// STAGE 2 — boot analysis over the core crates (background, non-gating).
///
/// Why: The analyze daemon health gates the console's analyze tab, but the
/// analysis *content* is advisory and never gates boot (RESOLVED Q6).
///
/// What: With `AnalyzeMode::Skip` → a `Skipped` report. Otherwise ensures the
/// analyze daemon; the daemon being down is `Degraded` (non-hard), not `Failed`.
/// `Blocking` vs `Background` only changes the detail label (the actual snapshot
/// scheduling is a passthrough to trusty-analyze).
///
/// Test: `super::tests::sequence_clean_all_ok` (background),
/// `sequence_analyze_skip`.
fn stage_analyze(
    members: &[BootMember],
    runner: &dyn Runner,
    policy: &ResolvedBootPolicy,
) -> StageReport {
    if policy.analyze_core == AnalyzeMode::Skip {
        return StageReport::new(
            "analyze",
            StageStatus::Skipped,
            "skipped (--analyze-core skip)",
        );
    }
    let analysis = members_in_stage(members, BootStage::Analysis);
    let Some(daemon) = analysis.first() else {
        return StageReport::new(
            "analyze",
            StageStatus::Skipped,
            "no analysis member in manifest",
        );
    };
    let outcome = ensure_member(runner, daemon);
    let mode = match policy.analyze_core {
        AnalyzeMode::Blocking => "blocking snapshot",
        _ => "background snapshot",
    };
    let targets = policy.analyze_targets.join(",");
    if outcome.gated_ok {
        StageReport::new(
            "analyze",
            StageStatus::Ok,
            format!(
                "daemon {} ; {mode} over [{targets}] (non-gating)",
                outcome.action
            ),
        )
    } else {
        // Analyze daemon down is degraded, not a hard failure (§5).
        StageReport::new(
            "analyze",
            StageStatus::Degraded,
            format!(
                "analyze daemon down ({}); analysis content absent",
                outcome.action
            ),
        )
    }
}

/// STAGE 3 — start the console, even on partial core (RESOLVED Q2).
///
/// Why: The console is the operator's window to see/remediate a degraded core,
/// so it starts when *any* core member is healthy; only when the entire core is
/// absent is starting it pointless.
///
/// What: Ensures the console member when `any_core_healthy`; on success records
/// the console URL on the report and returns `Ok`/`Degraded`; when no core is
/// healthy, `Skipped`.
///
/// Test: `super::tests::sequence_console_starts_on_partial_core`.
fn stage_console(
    members: &[BootMember],
    runner: &dyn Runner,
    console: &dyn ConsoleLocator,
    any_core_healthy: bool,
    report: &mut UpReport,
) -> StageStatus {
    if !any_core_healthy {
        let s = StageReport::new(
            "console",
            StageStatus::Skipped,
            "skipped — no core member healthy",
        );
        let status = s.status;
        report.push(s);
        return status;
    }
    let console_members = members_in_stage(members, BootStage::Console);
    let Some(c) = console_members.first() else {
        report.push(StageReport::new(
            "console",
            StageStatus::Skipped,
            "no console member in manifest",
        ));
        return StageStatus::Skipped;
    };
    let outcome = ensure_member(runner, c);
    if outcome.gated_ok {
        let url = console.console_url();
        report.console_url = url.clone();
        let detail = match url {
            Some(u) => format!("console up ({}) at {u}", outcome.action),
            None => format!("console up ({}); URL not yet discoverable", outcome.action),
        };
        report.push(StageReport::new("console", StageStatus::Ok, detail));
        StageStatus::Ok
    } else {
        // Console down → degraded (CLI still usable), exit 2 (§5).
        report.push(StageReport::new(
            "console",
            StageStatus::Degraded,
            format!("console down ({}); stack usable via CLI", outcome.action),
        ));
        StageStatus::Degraded
    }
}

/// STAGE 4 (+4b) — opt-in orchestrator and its Claude Code runtime.
///
/// Why: The session manager is heavier and not every boot wants it (§4); it is
/// started only when opted in, and then its native Claude Code runtime is
/// ensured present + at latest (§6) before declaring it ready.
///
/// What: On `OrchestratorDecision::Off`/`Prompt` (Prompt is resolved to a value
/// by mod.rs before this is called, so only On/Off reach here) records the right
/// report. On `On`: ensures the orchestrator member; if it has a `claude-code`
/// runtime, runs STAGE 4b. A missing orchestrator binary guides (not installed);
/// a runtime miss degrades the stage but does not fail the whole boot (§5).
///
/// Test: `super::tests::sequence_optin_on_ensures_runtime`,
/// `sequence_optin_off_skips_orchestrator`.
fn stage_orchestrator(
    members: &[BootMember],
    policy: &ResolvedBootPolicy,
    runner: &dyn Runner,
    orch: &OrchestratorDeps<'_>,
    report: &mut UpReport,
) {
    if policy.orchestrator != OrchestratorDecision::On {
        report.push(StageReport::new(
            "orchestrator",
            StageStatus::Skipped,
            "opt-in: not enabled (use --with-mpm or boot.with_mpm=true)",
        ));
        return;
    }
    let orch_members = members_in_stage(members, BootStage::Orchestrator);
    let Some(m) = orch_members.first() else {
        report.push(StageReport::new(
            "orchestrator",
            StageStatus::Skipped,
            "no orchestrator member in manifest",
        ));
        return;
    };

    let outcome = ensure_member(runner, m);
    if outcome.health == MemberHealth::NotInstalled {
        // Opt-in component absent → guide, do not auto-install (RESOLVED Q1).
        report.push(StageReport::new(
            "orchestrator",
            StageStatus::Degraded,
            format!(
                "{} not installed — run `tctl install {}` (not auto-installed)",
                m.id, m.id
            ),
        ));
        return;
    }
    if !outcome.gated_ok {
        report.push(StageReport::new(
            "orchestrator",
            StageStatus::Degraded,
            format!("{} not healthy ({})", m.id, outcome.action),
        ));
        return;
    }

    // STAGE 4b — ensure the native Claude Code runtime (only for claude-code).
    match &m.runtime {
        Some(rt) if rt.kind == "claude-code" => {
            let c = ensure_claude_code(
                orch.claude,
                &rt.install_hint,
                &rt.min_policy,
                policy.skip_claude_upgrade,
            );
            let status = if c.ready {
                StageStatus::Ok
            } else {
                StageStatus::Degraded
            };
            report.push(StageReport::new(
                "orchestrator",
                status,
                format!("{} healthy; claude-code: {} ({})", m.id, c.detail, c.action),
            ));
        }
        _ => {
            report.push(StageReport::new(
                "orchestrator",
                StageStatus::Ok,
                format!("{} healthy ({})", m.id, outcome.action),
            ));
        }
    }
}
