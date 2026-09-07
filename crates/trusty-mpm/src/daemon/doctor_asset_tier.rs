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
//! It also reports a second, independent fault (#4698): an agent file whose
//! `provenance:` frontmatter contradicts what the deployed-agent manifest
//! recorded for it. That check runs over the CANONICAL deploy directory as well
//! as the two non-canonical tiers — the shadowing scan above skips the
//! canonical one by construction, and it is precisely where a hand-edited
//! DEPLOYED agent lives.
//!
//! Test: `crates/trusty-mpm/src/daemon/doctor_asset_tier_tests.rs`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use trusty_agents_common::agents::tier_audit::TierAuditError;
use trusty_agents_common::agents::tier_audit::{
    MisplacedAgent, ProvenanceDisagreement, audit_agent_tier, audit_provenance,
};

// #4448: the roster moved to `core::bundled_roster` so the quarantine in
// `session_launch` shares this exact one — report and repair must agree.
use crate::core::bundled_roster::bundled_roster;
use crate::core::doctor::{CheckStatus, DoctorCheck};
use crate::core::paths::FrameworkPaths;

/// Name of this check as it appears in `tm doctor` output.
const CHECK_NAME: &str = "asset_tier";

/// How many offending agent names the message lists before summarising.
const MAX_NAMED: usize = 5;

/// One non-canonical tier and what the scan established about it.
struct TierScan {
    /// Human label for the message (`project`, `operator home`).
    label: &'static str,
    /// The directory the scan was asked for.
    dir: PathBuf,
    /// What the scan found, or why it could not look.
    outcome: TierOutcome,
}

/// What one tier scan established.
///
/// Why (#5626): the probe's message asserts `(scanned: <dir>)`, so "found
/// nothing" and "could not look" must not arrive as the same empty vec — that
/// is #4408's defining property, a check no failure can reach, reinstated by a
/// permission error.
/// What: [`Scanned`](Self::Scanned) carries the tm-owned files, possibly none;
/// [`Unscannable`](Self::Unscannable) carries the rendered
/// [`TierAuditError`].
/// Test: `verdict_unscannable_tier_is_warn`,
/// `verdict_unscannable_tier_is_not_reported_as_scanned`.
enum TierOutcome {
    /// The directory was enumerated; these are the tm-owned files in it.
    Scanned(Vec<MisplacedAgent>),
    /// The directory could not be enumerated; nothing is known about it.
    Unscannable(String),
}

impl TierScan {
    /// The tm-owned files found here, or none when the scan never ran.
    fn found(&self) -> &[MisplacedAgent] {
        match &self.outcome {
            TierOutcome::Scanned(found) => found,
            TierOutcome::Unscannable(_) => &[],
        }
    }
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
/// [`FrameworkPaths::agent_deploy_dir`] itself and any duplicate path. It then
/// runs [`audit_provenance`] over those tiers AND the canonical one (#4698) —
/// which the shadowing scan skips, and which is where a hand-edited deployed
/// agent lives. Verdict per [`verdict`].
/// Test: `project_tier_stub_on_a_bundled_name_fails`,
/// `clean_project_tier_is_ok`, `custom_project_agent_does_not_fire`,
/// `user_owned_project_agent_on_a_bundled_name_does_not_fire`,
/// `check_asset_tier_reports_a_hand_edited_deployed_agent`.
pub(super) fn check_asset_tier(
    paths: &FrameworkPaths,
    project_dir: Option<&Path>,
    home: &Path,
) -> DoctorCheck {
    let roster = bundled_roster(paths);
    let canonical = paths.agent_deploy_dir();

    let mut scans: Vec<TierScan> = Vec::new();
    // #4698: `provenance:` claims that contradict the ledger, from every tier.
    let mut disagreements: Vec<ProvenanceDisagreement> = Vec::new();
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
        // #5626: a tier that could not be enumerated is recorded as such, not
        // folded into "scanned, found nothing".
        let outcome = match audit_agent_tier(&dir, &roster) {
            Ok(found) => TierOutcome::Scanned(found),
            Err(e @ TierAuditError::Unscannable { .. }) => TierOutcome::Unscannable(e.to_string()),
        };
        // #4698: a hand-edited `provenance:` is a fact about a file wherever it
        // sits, so the same directory is checked for one.
        disagreements.extend(audit_provenance(&dir).unwrap_or_default());
        scans.push(TierScan {
            label,
            dir,
            outcome,
        });
    }

    // #4698: and the CANONICAL tier above all — it is skipped by the shadowing
    // scan above (every file there is tm-owned, correctly and uselessly), yet it
    // is exactly where a hand-edited DEPLOYED agent lives. A disagreement check
    // that could not look there would miss the case it exists for.
    disagreements.extend(audit_provenance(&canonical).unwrap_or_default());
    disagreements.sort_by(|a, b| a.path.cmp(&b.path));

    verdict(&scans, &canonical, &disagreements)
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
///
/// #5626: a tier that could not be enumerated never appears in the `scanned:`
/// list, and an otherwise-clean run holding one is `Warn`, not `Ok` — the
/// question is open, not answered. A real hit still outranks it, since a
/// confirmed shadowing is the more actionable finding.
///
/// #4698: `disagreements` never changes the severity a placement hit produced —
/// shadowing stays the more actionable finding — but it is always named in the
/// message, and it alone raises an otherwise-clean run from `Ok` to `Warn`. The
/// manifest still wins for ownership, so nothing resolves wrongly; what the
/// operator needs to know is that something other than a deploy edited the file.
/// Test: `verdict_project_hit_is_fail`, `verdict_home_only_is_warn`,
/// `verdict_clean_is_ok`, `verdict_names_the_files_and_both_tiers`,
/// `verdict_unscannable_tier_is_warn`,
/// `verdict_unscannable_tier_is_not_reported_as_scanned`,
/// `verdict_disagreement_alone_is_warn_not_ok`,
/// `verdict_reports_a_provenance_disagreement_alongside_shadowing`,
/// `verdict_clean_run_says_nothing_about_provenance`.
fn verdict(
    scans: &[TierScan],
    canonical: &Path,
    disagreements: &[ProvenanceDisagreement],
) -> DoctorCheck {
    let unscannable: Vec<String> = scans
        .iter()
        .filter_map(|s| match &s.outcome {
            TierOutcome::Unscannable(why) => Some(format!("{} tier — {why}", s.label)),
            TierOutcome::Scanned(_) => None,
        })
        .collect();

    let hits: Vec<&TierScan> = scans.iter().filter(|s| !s.found().is_empty()).collect();
    if hits.is_empty() {
        let scanned: Vec<String> = scans
            .iter()
            .filter(|s| matches!(s.outcome, TierOutcome::Scanned(_)))
            .map(|s| s.dir.display().to_string())
            .collect();
        if !unscannable.is_empty() {
            return DoctorCheck::new(
                CHECK_NAME,
                CheckStatus::Warn,
                format!(
                    "a non-canonical agent tier could not be scanned, so whether tm-owned agent \
                     files shadow the canonical tier {} is UNDETERMINED here: {}. Scanned: {}. \
                     Make the directory readable and re-run, or delete it.",
                    canonical.display(),
                    unscannable.join("; "),
                    if scanned.is_empty() {
                        "none".to_owned()
                    } else {
                        scanned.join(", ")
                    }
                ),
            );
        }
        // #4698: nothing is misplaced, but a file's own `provenance:` still
        // contradicts what the deployer recorded for it. The ledger wins for
        // ownership, so nothing is broken — but the file was edited by
        // something, and an operator who never hears that has no way to find
        // out. Not `Ok`.
        if !disagreements.is_empty() {
            return DoctorCheck::new(
                CHECK_NAME,
                CheckStatus::Warn,
                format!(
                    "no tm-owned agent files outside the canonical tier {}, but {}. The \
                     deployed-agent manifest wins for ownership, so agent resolution is \
                     unaffected — but the file's frontmatter was changed by something other \
                     than a deploy. Re-run `tm install` to restore it, or delete the file if \
                     it is yours (issue #4698).",
                    canonical.display(),
                    disagreement_list(disagreements)
                ),
            );
        }
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
    let mut detail: Vec<String> = hits
        .iter()
        .map(|s| {
            format!(
                "{} tier {} — {}",
                s.label,
                s.dir.display(),
                name_list(s.found())
            )
        })
        .collect();
    detail.extend(unscannable.iter().map(|u| format!("{u} (UNDETERMINED)")));
    // #4698: a disagreement never changes the severity below — shadowing is the
    // more actionable finding — but it is still reported rather than dropped.
    if !disagreements.is_empty() {
        detail.push(disagreement_list(disagreements));
    }

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

/// Render the `provenance:` disagreements, naming both records per file.
///
/// Why (#4698): the finding is only useful if it says WHICH file and WHAT the
/// two records disagree about — "a provenance mismatch was found" sends an
/// operator hunting. Each entry's `detail` already names the file, the
/// declaration, and the ledger's value, so this joins them rather than
/// re-deriving the wording and letting the two spellings drift.
/// What: up to [`MAX_NAMED`] entries, then a `(+N more)` tail, matching
/// [`name_list`]'s bound so one pathological directory cannot flood the report.
/// Test: `verdict_reports_a_provenance_disagreement_alongside_shadowing`,
/// `verdict_disagreement_alone_is_warn_not_ok`.
fn disagreement_list(disagreements: &[ProvenanceDisagreement]) -> String {
    let named: Vec<&str> = disagreements
        .iter()
        .take(MAX_NAMED)
        .map(|d| d.detail.as_str())
        .collect();
    let rest = disagreements.len().saturating_sub(named.len());
    let body = named.join("; ");
    let plural = if disagreements.len() == 1 { "" } else { "s" };
    if rest == 0 {
        return format!(
            "{} agent file{plural} declare a `provenance:` that contradicts the deployed-agent manifest: {body}",
            disagreements.len()
        );
    }
    format!(
        "{} agent file{plural} declare a `provenance:` that contradicts the deployed-agent manifest: {body} (+{rest} more)",
        disagreements.len()
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
