//! Doctor probe: tm-owned agents deployed OUTSIDE the canonical tier (#4442).
//!
//! Why: `check_agents` and `check_deployment_completeness` both look only at
//! the canonical destination, so they report a perfect green while a stale
//! copy of the SAME agent sits in a project's `.claude/agents/` and wins the
//! resolution race — the project tier outranks the user tier. That is #4408
//! observed live: a 32-byte project-tier stub beat the real 25 KB
//! `rust-engineer`, every delegation kept "succeeding", and no check could
//! fail. #4409 stopped tm writing per-workspace copies and retracts the ones it
//! tracks, but an untracked copy — hand-placed, or written by a binary older
//! than the ledger — is invisible to that retraction and lives forever.
//!
//! What: [`check_asset_tier`] scans the two non-canonical agent tiers (the
//! project's `.claude/agents/` when doctor is scoped to a project, and the
//! operator's `~/.claude/agents/`) and reports the tm-owned files in them.
//! Ownership is decided by
//! [`trusty_agents_common::agents::tier_audit`], the SHARED classifier the
//! #4448 quarantine consumes — report and repair must agree file-for-file, so
//! neither side re-derives the predicate. READ-ONLY: this probe never deletes,
//! moves, or rewrites anything.
//!
//! Test: `crates/trusty-mpm/src/daemon/doctor_asset_tier_tests.rs`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use trusty_agents_common::agents::tier_audit::{MisplacedAgent, audit_agent_tier};

// #4448: the roster moved to `core::bundled_roster` so the quarantine in
// `session_launch` shares this exact one — report and repair must agree.
use crate::core::bundled_roster::bundled_roster;
use crate::core::doctor::{CheckStatus, DoctorCheck};
use crate::core::paths::FrameworkPaths;

/// Name of this check as it appears in `tm doctor` output.
const CHECK_NAME: &str = "asset_tier";

/// How many offending agent names the message lists before summarising.
const MAX_NAMED: usize = 5;

/// One scanned non-canonical tier and what was found in it.
struct TierScan {
    /// Human label for the message (`project`, `operator home`).
    label: &'static str,
    /// The scanned directory.
    dir: PathBuf,
    /// tm-owned files found there, sorted by resolved agent name.
    found: Vec<MisplacedAgent>,
}

/// Probe for tm-owned agent files living outside the canonical deploy tier.
///
/// Why: see the module doc — this is the shadowing half of the deploy story,
/// which every presence-only check is structurally unable to fail on.
/// What: builds the canonical bundled-NAME roster (see [`bundled_roster`] — the
/// on-disk agent source UNION the agents embedded in this binary, so the roster
/// is never empty and the probe can never fall back to a false "nothing is
/// bundled" green), then scans `<project_dir>/.claude/agents/` and
/// `<home>/.claude/agents/`, skipping the canonical
/// [`FrameworkPaths::agent_deploy_dir`] itself and any duplicate path. Verdict
/// per [`verdict`].
/// Test: `project_tier_stub_on_a_bundled_name_fails`,
/// `clean_project_tier_is_ok`, `custom_project_agent_does_not_fire`,
/// `user_owned_project_agent_on_a_bundled_name_does_not_fire`.
pub(super) fn check_asset_tier(
    paths: &FrameworkPaths,
    project_dir: Option<&Path>,
    home: &Path,
) -> DoctorCheck {
    let roster = bundled_roster(paths);
    let canonical = paths.agent_deploy_dir();

    let mut scans: Vec<TierScan> = Vec::new();
    let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
    seen.insert(canonical.clone());
    for (label, dir) in [
        ("project", project_dir.map(agent_tier_of)),
        ("operator home", Some(agent_tier_of(home))),
    ] {
        let Some(dir) = dir else { continue };
        // The canonical tier is excluded by construction: every file there is
        // tm-owned, correctly and uselessly. A duplicate (project == home) is
        // scanned once.
        if !seen.insert(dir.clone()) {
            continue;
        }
        let found = audit_agent_tier(&dir, &roster);
        scans.push(TierScan { label, dir, found });
    }

    verdict(&scans, &canonical)
}

/// The `.claude/agents` tier under `base`.
fn agent_tier_of(base: &Path) -> PathBuf {
    base.join(".claude").join("agents")
}

/// Pure verdict over the scanned tiers.
///
/// Why: split out so both the firing and the clean branch are directly
/// testable, and so the severity rule is stated in one place.
/// What: `Fail` when the PROJECT tier holds tm-owned files — that tier
/// outranks the canonical one, so those files are actively shadowing and agent
/// resolution is already wrong. `Warn` when only the operator's home tier does:
/// a managed session relocates `CLAUDE_CONFIG_DIR`, so those copies are stale
/// rather than shadowing, and a plain `claude` run is the only thing that reads
/// them. `Ok` when neither holds any.
///
/// The `Fail` is deliberate and not negotiable down to `Warn`. Shadowing has no
/// degraded-but-usable state: the wrong agent definition loads, silently, and
/// every downstream symptom points somewhere else (#4408 cost a full debugging
/// session before the stub was found). It also matches the precedent set by
/// `check_agent_reachability` (#4451), the other probe about agents that exist
/// but do not resolve. And the empirical half: the deployer's skip-and-warn log
/// covering this same directory fired on eight separate days through
/// 2026-07-30 with no operator follow-up — a `Warn` here would join it.
/// Test: `verdict_project_hit_is_fail`, `verdict_home_only_is_warn`,
/// `verdict_clean_is_ok`, `verdict_names_the_files_and_both_tiers`.
fn verdict(scans: &[TierScan], canonical: &Path) -> DoctorCheck {
    let hits: Vec<&TierScan> = scans.iter().filter(|s| !s.found.is_empty()).collect();
    if hits.is_empty() {
        let scanned: Vec<String> = scans.iter().map(|s| s.dir.display().to_string()).collect();
        return DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Ok,
            format!(
                "no tm-owned agent files outside the canonical tier {} (scanned: {})",
                canonical.display(),
                if scanned.is_empty() {
                    "none".to_owned()
                } else {
                    scanned.join(", ")
                }
            ),
        );
    }

    let shadowing = hits.iter().any(|s| s.label == "project");
    let detail: Vec<String> = hits
        .iter()
        .map(|s| {
            format!(
                "{} tier {} — {}",
                s.label,
                s.dir.display(),
                name_list(&s.found)
            )
        })
        .collect();

    if shadowing {
        return DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Fail,
            format!(
                "tm-owned agent files live outside the canonical tier {}: {}. Claude Code \
                 resolves the PROJECT tier first, so these copies win over the deployed \
                 roster and the real agent never loads — a stale or stub definition runs \
                 under the right name and nothing reports it (issue #4408). Remove them: \
                 `tm install --reset-agents --reset-agents-workspaces` retracts the \
                 manifest-tracked ones; an untracked copy must be deleted by hand.",
                canonical.display(),
                detail.join("; ")
            ),
        );
    }

    DoctorCheck::new(
        CHECK_NAME,
        CheckStatus::Warn,
        format!(
            "tm-owned agent files linger in a legacy tier: {}. A managed session relocates \
             CLAUDE_CONFIG_DIR, so these do NOT shadow the canonical tier {} — but no deploy \
             refreshes them either, and a plain `claude` run outside tm reads them. Delete \
             them once.",
            detail.join("; "),
            canonical.display()
        ),
    )
}

/// Render up to [`MAX_NAMED`] agent names, summarising any remainder.
fn name_list(found: &[MisplacedAgent]) -> String {
    let named: Vec<&str> = found
        .iter()
        .take(MAX_NAMED)
        .map(|f| f.name.as_str())
        .collect();
    let rest = found.len().saturating_sub(named.len());
    if rest == 0 {
        return named.join(", ");
    }
    format!("{} (+{rest} more)", named.join(", "))
}

#[cfg(test)]
#[path = "doctor_asset_tier_tests.rs"]
mod tests;
