//! `tm doctor` scaffold-tracking probe: harness paths BOTH tracked in git AND
//! regenerated locally (issue #3427, Option 2 — detect + exact remediation).
//!
//! Why: split out of `doctor.rs` to keep it under the 500-SLOC production
//! cap, mirroring the existing `doctor_output_style.rs` / `doctor_fs_checks.rs`
//! / `doctor_hooks_hygiene.rs` splits. [`crate::core::scaffold_gitignore`] is
//! Option 1 (prevent FUTURE commits via `.gitignore`) — it cannot fix a
//! project that ALREADY committed `.claude/agents/`, `.claude/skills/`, or
//! `.claude/output-styles/` content, because appending a `.gitignore` entry
//! does not untrack anything already in the index. This probe detects that
//! already-broken state and prints the exact `git rm -r --cached` remediation,
//! mirroring what actually fixed the verified reproduction
//! (`duettoresearch/duetto-eve-agents`, worktree `tm-duetto-eve-agents-01`,
//! stranded 28 commits — duetto-eve-agents#111, commit 0e699e2).
//!
//! The reproduction's error output (`git merge --ff-only` "would be
//! overwritten") listed far more files than the true 7-file collision,
//! because git's message lists ALL untracked content anywhere in the
//! affected tree — that breadth is what "invited over-broad cleanup" per the
//! issue. This probe never parses or echoes that git error message; instead
//! it computes the TRUE INTERSECTION directly: [`tracked_scaffold_paths`]
//! (`git ls-files`, scoped to the three harness subtrees) ∩
//! [`regenerated_scaffold_paths`] (the exact relative paths tm's own
//! manifests/bundle say it writes into `project_dir`) — and reports ONLY
//! that intersection. A project's own hand-placed, intentionally-tracked
//! "project-custom" skill (`skills::tiers::SkillTier::Project`, issue #2816 —
//! on disk under `.claude/skills/<name>/` but ABSENT from the skill
//! manifest's `managed` keys) is therefore never flagged: it is tracked but
//! not tm-regenerated, so it falls outside the intersection by construction.
//!
//! What: [`check_scaffold_tracking`] is `Ok` when `project_dir` is absent,
//! is not a git working tree, or the git-tracked ∩ tm-regenerated
//! intersection is empty; `Warn` (never auto-modifying the index) naming the
//! exact colliding paths plus a copy-pasteable `git rm -r --cached …`
//! command and the `.gitignore` entries [`crate::core::scaffold_gitignore`]
//! already knows to add. Warn-only by design (issue #3427 explicit
//! requirement): `git rm --cached` on the wrong path set is destructive to
//! the user's tracking state, so this probe never runs it — it only ever
//! reports what a human (or a follow-up, explicitly-invoked command) should
//! run.
//! Test: the `tests` module below covers the true-intersection computation
//! (collision reported, unrelated tracked project-custom skill NOT reported),
//! the clean-repo no-false-positive case, and the non-git-repo no-op case.

use std::collections::BTreeSet;
use std::path::Path;

use crate::core::agent_manifest::AgentManifest;
use crate::core::doctor::{CheckStatus, DoctorCheck};
use crate::core::skill_manifest::SkillManifest;

/// Relative (forward-slash, `.claude`-rooted) paths tm's own manifests and
/// bundled catalog say it regenerates inside `project_dir`.
///
/// Why: this is the authoritative "paths tm regenerates" side of the
/// intersection — sourced directly from the SAME on-disk records the
/// deployers themselves maintain (`AgentManifest`/`SkillManifest`'s `managed`
/// keys) plus the fixed bundled output-style file list, rather than
/// re-deriving a full harness plan. A project-custom skill (on disk but
/// absent from `managed`) is correctly excluded here — tm never rewrites it,
/// so it is not part of "what tm regenerates" regardless of its git state.
/// What: agent paths as `.claude/agents/<managed filename>`; skill paths as
/// `.claude/skills/<stem>/SKILL.md` for a bare managed stem, or
/// `.claude/skills/<managed key>` when the key already carries the
/// `references/<file>` suffix `skills::deployer::deploy_one_file` writes
/// (mirrors that function's own `target_path` construction exactly). Output
/// styles are unconditional (issue #2125 deploys them into every project
/// tier every session, no manifest involved).
/// Test: `regenerated_paths_includes_managed_agent`,
/// `regenerated_paths_includes_skill_entry_and_references`,
/// `regenerated_paths_includes_all_output_styles`.
fn regenerated_scaffold_paths(project_dir: &Path) -> BTreeSet<String> {
    let mut paths = BTreeSet::new();

    let agent_manifest = AgentManifest::load(&project_dir.join(".claude").join("agents"));
    for filename in agent_manifest.managed.keys() {
        paths.insert(format!(".claude/agents/{filename}"));
    }

    let skill_manifest = SkillManifest::load(&project_dir.join(".claude").join("skills"));
    for key in skill_manifest.managed.keys() {
        if key.contains('/') {
            paths.insert(format!(".claude/skills/{key}"));
        } else {
            paths.insert(format!(".claude/skills/{key}/SKILL.md"));
        }
    }

    for style in crate::core::bundle::OUTPUT_STYLES {
        paths.insert(format!(".claude/output-styles/{}", style.file_name));
    }

    paths
}

/// Paths under the three harness subtrees that `git` tracks in `project_dir`.
///
/// Why: scoping the `git ls-files` pathspec to exactly the three harness
/// subtrees (rather than a bare `git ls-files` over the whole repo) keeps
/// this fast even on a large monorepo and returns paths already relative to
/// `project_dir` (the `-C` cwd), matching [`regenerated_scaffold_paths`]'s
/// format directly with no further normalization.
/// What: `None` when `project_dir` is not inside a git working tree (or
/// `git` itself is unavailable) — the caller treats that as "nothing to
/// check", not a probe failure. `Some(set)` (possibly empty) otherwise.
/// Test: `tracked_scaffold_paths_returns_none_outside_git_repo`,
/// `tracked_scaffold_paths_lists_committed_files`.
fn tracked_scaffold_paths(project_dir: &Path) -> Option<BTreeSet<String>> {
    if !project_dir.join(".git").exists() {
        return None;
    }
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(project_dir)
        .args([
            "ls-files",
            "--",
            ".claude/agents",
            ".claude/skills",
            ".claude/output-styles",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    Some(text.lines().map(str::to_owned).collect())
}

/// Render the exact, copy-pasteable remediation for a non-empty collision
/// set.
///
/// Why: the issue's explicit failure mode is a misleadingly BROAD git error
/// message inviting over-broad cleanup; the fix is a command scoped to
/// exactly the true intersection, nothing more.
/// What: one `git rm -r --cached <paths…>` line (sorted, space-joined) plus a
/// reminder that `tm` now auto-manages the matching `.gitignore` block
/// (issue #3427 Part 1) so this collision does not recur once the cleanup
/// commit lands.
/// Test: `remediation_message_lists_exact_paths`.
fn remediation_message(collisions: &BTreeSet<String>) -> String {
    let joined = collisions
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "{count} harness-scaffolding path{plural} {verb} BOTH tracked in git AND regenerated \
         locally by tm — the next `git merge --ff-only` may abort with \"would be overwritten\" \
         (issue #3427). Fix: `git rm -r --cached {joined}`, commit that removal, then commit \
         the .gitignore entries tm keeps up to date automatically \
         (`.claude/agents/`, `.claude/skills/`, `.claude/output-styles/`). This lists ONLY the \
         true collision — not every untracked file git's own merge error would list.",
        count = collisions.len(),
        plural = if collisions.len() == 1 { "" } else { "s" },
        verb = if collisions.len() == 1 { "is" } else { "are" },
    )
}

/// Probe for harness-scaffolding paths tracked in git AND regenerated by tm
/// (issue #3427).
///
/// Why/What: see the module doc. Warn-only — never runs `git rm --cached`
/// itself; a wrong-path automated cleanup is exactly the destructive
/// footgun the issue warns against, so remediation stays a human (or
/// separately-invoked, explicit) action.
/// Test: `check_ok_when_no_project_dir`, `check_ok_when_not_a_git_repo`,
/// `check_ok_on_clean_repo`, `check_warns_on_true_collision_only`.
pub(super) fn check_scaffold_tracking(project_dir: Option<&Path>) -> DoctorCheck {
    let Some(project) = project_dir else {
        return DoctorCheck::new(
            "scaffold_tracking",
            CheckStatus::Ok,
            "no project directory supplied — scaffold-tracking check not applicable",
        );
    };

    let Some(tracked) = tracked_scaffold_paths(project) else {
        return DoctorCheck::new(
            "scaffold_tracking",
            CheckStatus::Ok,
            "project is not a git working tree — scaffold-tracking check not applicable",
        );
    };

    let regenerated = regenerated_scaffold_paths(project);
    let collisions: BTreeSet<String> = tracked.intersection(&regenerated).cloned().collect();

    if collisions.is_empty() {
        DoctorCheck::new(
            "scaffold_tracking",
            CheckStatus::Ok,
            "no harness-scaffolding paths are both tracked in git and regenerated by tm",
        )
    } else {
        DoctorCheck::new(
            "scaffold_tracking",
            CheckStatus::Warn,
            remediation_message(&collisions),
        )
    }
}

#[cfg(test)]
#[path = "doctor_scaffold_tracking_tests.rs"]
mod tests;
