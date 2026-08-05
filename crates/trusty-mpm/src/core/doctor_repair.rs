//! `tm doctor --fix`: the repair driver behind the diagnostic (issue #4948).
//!
//! Why: every doctor check was pull-only. On the machine that motivated #4948,
//! `legacy_sources` had been reporting 23 leftover `tm-*` skill copies,
//! `hooks_contamination` 26 project settings files, and `skill_staleness` one
//! repairable drift — for weeks. All three are `Warn`, none auto-repairs, and
//! all three only surface when a human happens to run `tm doctor`. A report
//! nobody can act on in the same breath is a report that gets ignored.
//!
//! `tm doctor --fix-skills` (#4604) already proved the shape a repair must
//! take here, so this module extends that shape rather than inventing a second
//! one: back up before overwriting, verify from disk, never silently overwrite
//! a hand-edited file, and never delete. What it adds is the preview
//! `--fix-skills` lacked and a registry so a second repairable check does not
//! mean a second bespoke flag.
//!
//! Three rules bound everything here, and they are the reason the module is
//! this small:
//!
//! 1. **Dry run is the default.** [`RepairMode::DryRun`] is what a bare
//!    `--fix` gets; writing requires `--yes`. Every repair must be able to
//!    describe itself, per item and with a path, before it acts.
//! 2. **Nothing is deleted, and nothing the operator authored is rewritten.**
//!    A checksum mismatch means a human edited the file, which is ownership,
//!    not rot — the same rule `skill_repair` enforces for FROZEN skills and
//!    `push_guard` enforces for a foreign `pre-push` hook. This module honours
//!    it by delegating to those modules rather than re-deriving ownership.
//! 3. **`~/.claude/` is the operator's real Claude Code config.** tm refreshes
//!    the skill copies it deployed there and it removes the hook entries a
//!    pre-#2940 `tm install` wrote into project settings — both are tm's own
//!    residue, recorded in tm's own ledgers. It does NOT delete anything else
//!    there. [`refuse_legacy_sources`] is that refusal, made visible rather
//!    than silent: `legacy_sources` findings are REPORTED with their paths and
//!    a reason, never acted on. See #4409.
//!
//! What: [`RepairMode`], the [`RepairStep`] outcome model, and the three
//! repairs `--fix` drives — [`repair_hooks_contamination`],
//! [`repair_push_guard`], and (via [`crate::core::skill_repair`]) the skill
//! redeploy — plus [`refuse_legacy_sources`].
//! Test: `doctor_repair_tests.rs`.

use std::path::{Path, PathBuf};

use crate::core::push_guard::{GuardState, HookInstall, inspect_pre_push_guard,
    install_pre_push_guard};
use crate::core::standalone::hooks::cleanup::clean_settings_file;

/// Whether a repair may write, or is only describing what it would write.
///
/// Why: a command that rewrites operator state needs the preview and the apply
/// to be the same code path — a separate planner drifts from the actor exactly
/// when it matters. Threading a mode rather than duplicating the logic is what
/// makes "print exactly what would change before changing it" a guarantee
/// instead of an intention.
/// What: two variants; [`RepairMode::DryRun`] is the default a bare `--fix`
/// selects, and [`RepairMode::Apply`] requires `--yes`.
/// Test: every `*_dry_run_*` test in `doctor_repair_tests.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepairMode {
    /// Describe what would change. Write nothing.
    DryRun,
    /// Perform the repair.
    Apply,
}

impl RepairMode {
    /// Pick the mode a CLI flag pair selects.
    ///
    /// Why: `--fix` alone must never write, so the conversion lives in one
    /// place rather than as an `if apply { … } else { … }` at each call site.
    /// What: [`RepairMode::Apply`] only when `apply` is `true`.
    /// Test: `mode_defaults_to_dry_run`.
    pub fn from_apply_flag(apply: bool) -> Self {
        if apply { Self::Apply } else { Self::DryRun }
    }
}

/// What one repair did — or deliberately did not do — to one file.
///
/// Why: "would change", "changed", "refused" and "failed" are four different
/// facts and collapsing any pair of them is how a fix mode lies. `Refused` in
/// particular is the safety rule working, not an error, and must never render
/// like one.
/// What: four terminal states. `Applied` carries the backup so an operator can
/// undo it; `Refused` carries the reason.
/// Test: `doctor_repair_tests.rs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepStatus {
    /// Dry run: this WOULD change, and nothing was written.
    Planned,
    /// Applied, with the backup taken beforehand when one was possible.
    Applied { backup: Option<PathBuf> },
    /// Deliberately not repaired; the string says why.
    Refused(String),
    /// Attempted and did not work; the string says why.
    Failed(String),
}

/// One repair's outcome against one path.
///
/// Why: an operator triaging `tm doctor --fix` output needs to map every line
/// back to the check that produced it and the file it concerns, without
/// re-running the diagnostic.
/// What: the originating check name, the exact path, a one-line description of
/// the change, and the [`StepStatus`].
/// Test: `doctor_repair_tests.rs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepairStep {
    /// The `tm doctor` check this repair answers (`hooks_contamination`, …).
    pub check: &'static str,
    /// The file or directory the step concerns.
    pub path: PathBuf,
    /// One line describing the change, in the same words for both modes.
    pub what: String,
    /// What happened.
    pub status: StepStatus,
}

impl RepairStep {
    /// Did this step actually modify the filesystem?
    ///
    /// Why: the summary counts applied repairs and must not count a plan, a
    /// refusal, or a failure as one.
    /// What: `true` only for [`StepStatus::Applied`].
    /// Test: `hooks_repair_applies_and_backs_up`.
    pub fn changed(&self) -> bool {
        matches!(self.status, StepStatus::Applied { .. })
    }
}

/// Remove tm's own hook entries from a project's `.claude/settings*.json`.
///
/// Why (#2940, #4948): a pre-#2940 `tm install` wired tm hooks into every
/// project settings file it could reach, and `hooks_contamination` has been
/// reporting the leftovers ever since without being able to clear them. The
/// entries are tm's — tm wrote them, tm recognises them, and removing them
/// restores a file the project owns to what the project put there.
///
/// It removes ONLY entries [`crate::core::standalone::hooks`] classifies as
/// tm-owned. A foreign (claude-mpm) entry in the same file, or even in the same
/// hand-mixed hook group, is left exactly where it is — that call belongs to
/// the operator, and `hooks_foreign_conflict` stays report-only for the same
/// reason.
/// What: scans `<project>/.claude/settings.json` and `settings.local.json`
/// through [`clean_settings_file`], whose `force` parameter IS the dry-run
/// gate, so preview and apply share one reader and one classifier. Applying
/// writes a byte-identical `<path>.bak-<epoch>` first. A file with no tm
/// entries produces no step. Scope is this project only — the `$HOME`-wide
/// sweep stays `tm hooks clean`, which pays that cost once on request rather
/// than on every diagnostic.
/// Test: `hooks_repair_applies_and_backs_up`,
/// `hooks_repair_dry_run_changes_nothing`,
/// `hooks_repair_leaves_foreign_entries_alone`.
pub fn repair_hooks_contamination(project_dir: &Path, mode: RepairMode) -> Vec<RepairStep> {
    let claude = project_dir.join(".claude");
    let mut steps = Vec::new();
    for name in ["settings.json", "settings.local.json"] {
        let path = claude.join(name);
        match clean_settings_file(&path, mode == RepairMode::Apply) {
            Ok(None) => {}
            Ok(Some(outcome)) => {
                let what = format!(
                    "remove tm hook entries under [{}]",
                    outcome.removed_events.join(", ")
                );
                let status = match mode {
                    RepairMode::DryRun => StepStatus::Planned,
                    RepairMode::Apply => StepStatus::Applied {
                        backup: outcome.backup_path,
                    },
                };
                steps.push(RepairStep {
                    check: "hooks_contamination",
                    path: outcome.path,
                    what,
                    status,
                });
            }
            Err(e) => steps.push(RepairStep {
                check: "hooks_contamination",
                path,
                what: "remove tm hook entries".to_string(),
                status: StepStatus::Failed(e.to_string()),
            }),
        }
    }
    steps
}

/// Install the #2867 cross-branch `pre-push` guard on this clone.
///
/// Why (#2867, #4948): the guard installs itself only on the clone path, so
/// every base clone that predates it is silently unprotected and a worktree
/// tracking a foreign branch can force-push over that branch's reviewed
/// lineage. `push_guard` has been naming `tm repair push-guard` as the
/// retrofit; this makes `--fix` run it. The repair is purely additive — it
/// writes one file that was absent or that tm itself wrote an older revision
/// of — which is why it is in the auto-repairable set at all.
///
/// It never overwrites a hook it does not own. A foreign `pre-push`, a
/// symlink, an unreadable file, or a `core.hooksPath` redirect each produce a
/// [`StepStatus::Refused`] naming the reason, because
/// [`inspect_pre_push_guard`] and [`install_pre_push_guard`] make one shared
/// ownership judgement rather than two that could drift.
/// What: classifies the slot with [`inspect_pre_push_guard`] — read-only, and
/// the whole of the dry run — then in [`RepairMode::Apply`] calls
/// [`install_pre_push_guard`]. An already-current guard produces no step.
/// Test: `push_guard_repair_installs_when_missing`,
/// `push_guard_repair_dry_run_writes_nothing`,
/// `push_guard_repair_refuses_a_foreign_hook`.
pub fn repair_push_guard(repo_path: &Path, mode: RepairMode) -> Vec<RepairStep> {
    let what = match inspect_pre_push_guard(repo_path) {
        GuardState::Current(_) => return Vec::new(),
        GuardState::Missing(path) => (path, "install the cross-branch pre-push guard"),
        GuardState::Outdated(path) => (path, "upgrade the cross-branch pre-push guard"),
        GuardState::Foreign(reason) => {
            return vec![RepairStep {
                check: "push_guard",
                path: repo_path.to_path_buf(),
                what: "install the cross-branch pre-push guard".to_string(),
                status: StepStatus::Refused(reason),
            }];
        }
    };
    let (path, description) = what;

    if mode == RepairMode::DryRun {
        return vec![RepairStep {
            check: "push_guard",
            path,
            what: description.to_string(),
            status: StepStatus::Planned,
        }];
    }

    let status = match install_pre_push_guard(repo_path) {
        Ok(HookInstall::Installed(_)) => StepStatus::Applied { backup: None },
        // Between the inspection above and this call the slot became current
        // or foreign. Report what actually happened, never what was planned.
        Ok(HookInstall::AlreadyCurrent(_)) => {
            StepStatus::Refused("already current — nothing to do".to_string())
        }
        Ok(HookInstall::Refused(reason)) => StepStatus::Refused(reason),
        Err(e) => StepStatus::Failed(e),
    };
    vec![RepairStep {
        check: "push_guard",
        path,
        what: description.to_string(),
        status,
    }]
}

/// Report every `legacy_sources` finding as REFUSED — never delete one.
///
/// Why (#4948, #4409): this is the check whose "obvious" repair is the one
/// that must not ship. `legacy_sources` counts `~/.claude/skills/tm-*` copies
/// and the legacy `~/.trusty-mpm/claude-config` directory; the repair that
/// would clear the warning is deletion, inside the operator's real Claude Code
/// config. Some of those copies are hand-edited — a checksum mismatch is
/// ownership, not rot — and tm cannot tell a stale duplicate from somebody's
/// customization by looking at a directory name. A fix mode that deletes what
/// it thinks is orphaned is the 2026-07-21 `rm -rf .base/*` incident with a
/// scheduler: that command wiped a shared bare clone's git internals and
/// orphaned ~70 worktrees.
///
/// So the answer is "report, refuse, and say why", and it is implemented
/// rather than merely documented — an operator running `--fix` sees each path
/// and the reason it survived, instead of silence they could mistake for
/// "nothing found". Refreshing the CONTENT of a tm-deployed copy there is a
/// different operation and is still available: it goes through the skill
/// repair, which consults tm's own ownership ledger first.
/// What: one [`StepStatus::Refused`] step per `tm-*` entry directly under
/// `<home>/.claude/skills`, plus one for the legacy managed-config directory
/// if present. Reads directory names only; opens nothing; writes nothing.
/// Test: `legacy_sources_are_refused_never_deleted`,
/// `legacy_sources_ignores_a_foreign_skill`.
pub fn refuse_legacy_sources(home: &Path) -> Vec<RepairStep> {
    const REASON: &str = "tm never deletes inside ~/.claude — a copy here may be hand-edited, \
                          and a directory name cannot prove otherwise. Remove it by hand once \
                          you have confirmed nothing depends on it";

    let mut steps = Vec::new();
    let skills = home.join(".claude").join("skills");
    if let Ok(entries) = std::fs::read_dir(&skills) {
        let mut legacy: Vec<PathBuf> = entries
            .flatten()
            .filter(|e| {
                e.file_name()
                    .to_str()
                    .is_some_and(|n| n.starts_with("tm-"))
            })
            .map(|e| e.path())
            .collect();
        legacy.sort();
        steps.extend(legacy.into_iter().map(|path| RepairStep {
            check: "legacy_sources",
            path,
            what: "leftover pre-migration tm-* skill copy".to_string(),
            status: StepStatus::Refused(REASON.to_string()),
        }));
    }

    let legacy_config = home.join(".trusty-mpm").join("claude-config");
    if legacy_config.is_dir() {
        steps.push(RepairStep {
            check: "legacy_sources",
            path: legacy_config,
            what: "legacy managed-config directory".to_string(),
            status: StepStatus::Refused(
                "superseded by the tm-owned ~/.trusty-tools config home, but tm does not \
                 delete a directory it can no longer attribute. Remove it by hand once no \
                 session references it"
                    .to_string(),
            ),
        });
    }
    steps
}

#[cfg(test)]
#[path = "doctor_repair_tests.rs"]
mod tests;
