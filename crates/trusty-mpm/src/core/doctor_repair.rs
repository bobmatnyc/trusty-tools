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
//! What: [`RepairMode`], the [`RepairStep`] outcome model, and the repairs
//! `--fix` drives — [`repair_build_tree_binary`] (#7262, the repoint that runs
//! first), [`repair_hooks_contamination`], [`repair_missing_hook_group`]
//! (#7490 — runs after that strip and restores what a launch would write),
//! [`repair_push_guard`], [`repair_output_style`] (#5866 — the one check whose
//! remedy string named a command with no such step), and (via
//! [`crate::core::skill_repair`]) the skill redeploy — plus
//! [`refuse_legacy_sources`].
//! Test: `doctor_repair_tests.rs`.

use std::path::{Path, PathBuf};

use crate::core::push_guard::{
    GuardState, HookInstall, inspect_pre_push_guard, install_pre_push_guard,
};
use crate::core::standalone::hooks::cleanup::clean_settings_file;
use crate::core::standalone::hooks::repoint::{build_tree_commands_in, repoint_settings_file};
use crate::core::standalone::hooks::{StableHookExeError, resolve_stable_hook_exe};

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

/// Appended to a `hooks_contamination` step that removes the PM guard.
///
/// Why (#7262): the base message names the EVENTS it cleans, which reads as a
/// tidy-up. Removing the `PreToolUse` PM-guard entry is not a tidy-up — it
/// takes PM enforcement offline, and this repair does not put it back. Only
/// the project's next managed `tm` launch re-renders the hook, so an operator
/// who runs `--fix` and then keeps working in an unmanaged session is
/// unguarded with nothing on screen saying so.
/// What: one clause, in both dry-run and apply wording (the step's `what` is
/// mode-independent by [`RepairStep`]'s contract), appended to the base
/// message when
/// [`crate::core::standalone::hooks::cleanup::CleanOutcome::removed_pm_guard`]
/// is set.
/// Test: `hooks_repair_warns_when_it_removes_the_pm_guard`.
const PM_GUARD_REMOVAL_CLAUSE: &str =
    "; pm-guard enforcement is absent until the project's next managed `tm` launch re-renders it";

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
/// `hooks_repair_leaves_foreign_entries_alone`,
/// `hooks_repair_warns_when_it_removes_the_pm_guard`,
/// `hooks_repair_omits_the_pm_guard_clause_for_other_entries`.
pub fn repair_hooks_contamination(project_dir: &Path, mode: RepairMode) -> Vec<RepairStep> {
    let claude = project_dir.join(".claude");
    let mut steps = Vec::new();
    for name in ["settings.json", "settings.local.json"] {
        let path = claude.join(name);
        match clean_settings_file(&path, mode == RepairMode::Apply) {
            Ok(None) => {}
            Ok(Some(outcome)) => {
                let mut what = format!(
                    "remove tm hook entries under [{}]",
                    outcome.removed_events.join(", ")
                );
                // #7262: removing the guard is not self-describing — say what
                // it costs and when it comes back.
                if outcome.removed_pm_guard {
                    what.push_str(PM_GUARD_REMOVAL_CLAUSE);
                }
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

/// The `tm doctor` check [`repair_missing_hook_group`] answers.
///
/// Why: named once so the repair, its tests, and the driver's ordering comment
/// cannot drift onto a check name the report does not emit.
/// What: the string `hooks_missing_tm_group`, as
/// `daemon::doctor_hooks_hygiene` publishes it.
/// Test: `missing_group_repair_merges_the_sessionstart_group_back`.
const MISSING_GROUP_CHECK: &str = "hooks_missing_tm_group";

/// Merge the tm hook lifecycle groups back into a project settings file that
/// is missing one (issue #7490).
///
/// Why: `hooks_missing_tm_group` reports an event `tm hook` never fires for,
/// and nothing else puts it back — `prepare_session` is the only writer and no
/// resume or relaunch reached it before #7490. The operator's alternative was
/// a hand-edit of `.claude/settings.json`.
///
/// This runs AFTER [`repair_hooks_contamination`] in the driver, deliberately.
/// That pass STRIPS every `<exe> hook` entry it finds, including the ones this
/// pass exists to restore, so running first would report a merge the strip
/// then undid. Running last means a managed project ends the `--fix` in the
/// state its next launch would write, rather than with PM lifecycle hooks
/// offline until then.
/// What: one step for `<project>/.claude/settings.json` when
/// [`crate::core::session_launch::missing_lifecycle_hook_events`] names at
/// least one gap — no step for a complete file, a missing file, or one tm
/// never provisioned. Applying calls
/// [`crate::core::session_launch::ensure_project_hooks`], the SAME merge a
/// launch runs: tm's own prior entries are replaced by identity, every other
/// entry (the `trusty-memory inbox-check` under `SessionStart`) is preserved,
/// and the file is snapshotted before the atomic write. A refusal to resolve
/// an installed binary surfaces as [`StepStatus::Refused`], never a silent
/// skip.
/// Test: `missing_group_repair_merges_the_sessionstart_group_back`,
/// `missing_group_repair_dry_run_changes_nothing`,
/// `missing_group_repair_is_silent_for_a_complete_file`.
pub fn repair_missing_hook_group(project_dir: &Path, mode: RepairMode) -> Vec<RepairStep> {
    repair_missing_hook_group_with(project_dir, None, mode)
}

/// [`repair_missing_hook_group`] with the hook binary pinned by the caller.
///
/// Why (#7244's seam, reused): `resolve_stable_hook_exe` refuses a `cargo test`
/// harness binary, so a test driving the apply arm through
/// [`repair_missing_hook_group`] would assert the refusal rather than the
/// merge on any host without `tm` installed.
/// What: as [`repair_missing_hook_group`]; `exe_override` is `None` in
/// production.
/// Test: see [`repair_missing_hook_group`].
pub(crate) fn repair_missing_hook_group_with(
    project_dir: &Path,
    exe_override: Option<&Path>,
    mode: RepairMode,
) -> Vec<RepairStep> {
    let path = project_dir.join(".claude").join("settings.json");
    let Some(val) = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
    else {
        return Vec::new();
    };
    let gaps = crate::core::session_launch::missing_lifecycle_hook_events(&val);
    if gaps.is_empty() {
        return Vec::new();
    }
    let what = format!("merge the tm hook group back under [{}]", gaps.join(", "));
    let status = match mode {
        RepairMode::DryRun => StepStatus::Planned,
        RepairMode::Apply => {
            match crate::core::session_launch::ensure_project_hooks(project_dir, exe_override) {
                Ok(()) => StepStatus::Applied { backup: None },
                // #7490: an unresolvable installed binary is the fail-closed
                // rule working (#7244), not a failure — a step that wrote
                // nothing on purpose must not render like one that broke.
                Err(e @ crate::core::session_launch::PrepError::HookExe { .. }) => {
                    StepStatus::Refused(e.to_string())
                }
                Err(e) => StepStatus::Failed(e.to_string()),
            }
        }
    };
    vec![RepairStep {
        check: MISSING_GROUP_CHECK,
        path,
        what,
        status,
    }]
}

/// The `tm doctor` check [`repair_build_tree_binary`] answers.
///
/// Why: named once so the repair, its tests, and the driver's ordering comment
/// cannot drift onto a check name the report does not emit.
/// What: the string `hooks_build_tree_binary`, as
/// `daemon::doctor_hooks_hygiene` publishes it.
/// Test: `build_tree_repair_repoints_at_the_installed_binary`.
const BUILD_TREE_CHECK: &str = "hooks_build_tree_binary";

/// Repoint every build-tree hook and `statusLine` command at the installed
/// binary (issue #7262, reopened).
///
/// Why: #7286 made `hooks_build_tree_binary` visible and left it unrepairable —
/// `--fix` planned zero repairs for it, and on 2026-09-10 eight projects were
/// fixed by hand with `sed`. [`repair_hooks_contamination`] is not that repair:
/// it REMOVES the entry, which takes PM enforcement offline until the project's
/// next managed launch and cannot touch `statusLine.command` at all. Repointing
/// keeps the entry and fixes the one thing wrong with it.
///
/// This runs BEFORE [`repair_hooks_contamination`] in the driver, deliberately.
/// [`crate::core::standalone::hooks::is_mpm_hook_command`] claims a build-tree
/// command, so a contamination pass that ran first would delete the PM guard
/// this pass would have restored.
/// What: one step per file that carried at least one such command, naming the
/// count and the first command in full. Scope is the caller's — the driver
/// passes every settings file on the machine, not just this project's, because
/// the damage is written by whichever build tree ran last and lands wherever
/// that session was pointed. Applying takes a timestamped snapshot before it
/// writes; see [`repoint_settings_file`], which is fail-closed on an
/// unreadable, unparseable, or non-object file.
///
/// When no installed `tm`/`trusty-mpm` binary resolves there is nothing to
/// repoint to, so every affected file gets a [`StepStatus::Refused`] naming the
/// reason — a clean file stays silent, because a machine with no tm on `PATH`
/// must not grow a refusal line per project.
/// Test: `build_tree_repair_repoints_at_the_installed_binary`,
/// `build_tree_repair_dry_run_changes_nothing`,
/// `build_tree_repair_is_idempotent`,
/// `build_tree_repair_fails_closed_on_unparseable_json`,
/// `build_tree_repair_refuses_when_no_installed_binary_resolves`,
/// `build_tree_repair_is_silent_for_a_clean_file`.
pub fn repair_build_tree_binary(settings_files: &[PathBuf], mode: RepairMode) -> Vec<RepairStep> {
    repair_build_tree_binary_with(settings_files, resolve_stable_hook_exe(None), mode)
}

/// [`repair_build_tree_binary`] with the installed-binary resolution supplied.
///
/// Why: [`resolve_stable_hook_exe`] reads `current_exe()` and `PATH`, so a test
/// running inside a `cargo test` harness would get a different answer on a
/// machine that has `tm` installed than on one that does not — and the refusal
/// arm is the half worth pinning. Injecting the result makes both arms
/// deterministic without a second resolver.
/// What: as [`repair_build_tree_binary`]; `installed` is what that function
/// resolves in production.
/// Test: see [`repair_build_tree_binary`].
fn repair_build_tree_binary_with(
    settings_files: &[PathBuf],
    installed: Result<PathBuf, StableHookExeError>,
    mode: RepairMode,
) -> Vec<RepairStep> {
    settings_files
        .iter()
        .filter_map(|path| build_tree_step(path, installed.as_deref(), mode))
        .collect()
}

/// One file's worth of [`repair_build_tree_binary`].
///
/// Why: three outcomes (nothing to do, repointed, refused/failed) read far
/// better as one function than as three arms nested inside a loop.
/// What: `None` when the file carries no build-tree command. Otherwise the step
/// — `Refused` when `installed` is an error, `Failed` when the rewrite itself
/// failed, else `Planned`/`Applied`.
/// Test: see [`repair_build_tree_binary`].
fn build_tree_step(
    path: &Path,
    installed: Result<&Path, &StableHookExeError>,
    mode: RepairMode,
) -> Option<RepairStep> {
    let installed = match installed {
        Ok(bin) => bin,
        Err(e) => {
            // Speak up only for a file that actually carries the damage.
            if build_tree_commands_in(path).is_empty() {
                return None;
            }
            return Some(RepairStep {
                check: BUILD_TREE_CHECK,
                path: path.to_path_buf(),
                what: "repoint build-tree hook commands at the installed tm binary".to_string(),
                status: StepStatus::Refused(e.to_string()),
            });
        }
    };

    match repoint_settings_file(path, installed, mode == RepairMode::Apply) {
        Ok(None) => None,
        Ok(Some(outcome)) => {
            let (first, _) = outcome
                .hooks
                .first()
                .or(outcome.statusline.as_ref())
                .expect("an outcome exists only when at least one command was rewritten");
            let what = format!(
                "repoint {} command{} at {} (first: `{first}`)",
                outcome.command_count(),
                if outcome.command_count() == 1 {
                    ""
                } else {
                    "s"
                },
                installed.display(),
            );
            let status = match mode {
                RepairMode::DryRun => StepStatus::Planned,
                RepairMode::Apply => StepStatus::Applied {
                    backup: outcome.backup_path,
                },
            };
            Some(RepairStep {
                check: BUILD_TREE_CHECK,
                path: outcome.path,
                what,
                status,
            })
        }
        Err(e) => Some(RepairStep {
            check: BUILD_TREE_CHECK,
            path: path.to_path_buf(),
            what: "repoint build-tree hook commands at the installed tm binary".to_string(),
            status: StepStatus::Failed(e.to_string()),
        }),
    }
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

/// The `tm doctor --fix` invocation that applies an output-style repair.
///
/// Why (#5866): `output_style_staleness` and `check_output_style` both told the
/// operator to "run `tm install` to redeploy", and `tm install` has no
/// output-style step at all — its 366-line log named none, and the deployed
/// file kept its mtime across a full run. A remedy string is only as good as
/// the command it names, so the string and the repair that honours it are
/// pinned to one constant.
/// Test: `output_style_remedy_names_the_fix_command`.
pub const OUTPUT_STYLE_REMEDY: &str = "tm doctor --fix --yes";

/// Redeploy the operator's bundled output styles (#5866).
///
/// Why: nothing repaired this. `tm install` never touched output styles;
/// [`crate::core::session_launch`]'s `deploy_output_style` and
/// [`crate::core::output_style_deployer::deploy_output_styles`] both run only
/// on a session launch or a managed-config bootstrap, so an operator whose
/// `~/.claude/output-styles/` had drifted had no command to run. This is that
/// command, kept as narrow as the rest of this module: output styles are
/// framework-owned files tm wrote, with no operator-authored variant to
/// protect, which is why the redeploy is additive rather than gated on a
/// checksum ledger the way the skill repair is.
/// What: scans `<home>/.claude/output-styles/` with
/// [`crate::core::output_style_deployer::output_style_drift`] — the SAME scan
/// `check_output_style_staleness` reports from, so preview and report cannot
/// disagree — and emits one step per non-matching style. An unreadable file
/// produces [`StepStatus::Refused`]: an IO failure is not evidence of
/// staleness, and overwriting on it would destroy a file tm cannot read to
/// back up. In [`RepairMode::Apply`] a single
/// [`crate::core::output_style_deployer::deploy_output_styles`] call writes
/// every drifted and missing file atomically; each step then reports what that
/// call actually did rather than what was planned, PER STYLE — a sibling's IO
/// error cannot turn a completed write into `Failed` (#5866). Orphaned files
/// under `output-styles/` are deliberately NOT touched — `output_style_staleness`
/// names them and refuses to delete them, and so does this (issue #2333).
///
/// #7423: it now repairs EVERY [`crate::core::output_style_tiers::output_style_tiers`]
/// entry. The old body hardcoded `home.join(".claude")`, so
/// `tm doctor --fix --yes` on 1.5.29 redeployed the operator's copy and left
/// `$CLAUDE_CONFIG_DIR/output-styles/trusty-mpm.md` — the copy a tm-launched
/// session actually reads — at its old 12,012 bytes.
/// Test: `output_style_repair_plans_the_drifted_file`,
/// `output_style_repair_dry_run_writes_nothing`,
/// `output_style_repair_applies_and_reports_from_disk`,
/// `output_style_repair_is_empty_when_in_sync`,
/// `output_style_repair_refuses_an_unreadable_file`,
/// `one_unreadable_style_does_not_fail_a_sibling_that_was_written`,
/// `output_style_repair_rewrites_the_managed_tier`.
pub fn repair_output_style(
    home: &Path,
    managed_config: Option<&Path>,
    mode: RepairMode,
) -> Vec<RepairStep> {
    crate::core::output_style_tiers::output_style_tiers(home, managed_config)
        .into_iter()
        .flat_map(|tier| repair_output_style_tier(&tier.claude_dir, tier.label, mode))
        .collect()
}

/// [`repair_output_style`] for ONE tier's `CLAUDE_CONFIG_DIR`-shaped directory.
///
/// Why (#7423): the per-tier body is the pre-existing repair verbatim; splitting
/// it out is what lets the tier loop above exist without a second copy of the
/// drift/refusal/verify rules. The tier label rides into each step's `what` so
/// an operator reading the preview can tell the two `trusty-mpm.md` copies apart
/// before either is written.
/// What: the scan-then-deploy-then-report-from-disk pipeline documented on
/// [`repair_output_style`], against `<claude_dir>/output-styles/`.
/// Test: `repair_output_style`'s tests exercise it through the public entry.
fn repair_output_style_tier(
    claude: &Path,
    tier: &'static str,
    mode: RepairMode,
) -> Vec<RepairStep> {
    use crate::core::output_style_deployer::{
        StyleDrift, deploy_output_styles, output_style_drift,
    };

    let styles_dir = claude.join("output-styles");
    let drift = output_style_drift(&styles_dir);
    if drift.is_empty() {
        return Vec::new();
    }

    let writable: Vec<&(&'static str, StyleDrift)> = drift
        .iter()
        .filter(|(_, state)| !matches!(state, StyleDrift::Unreadable(_)))
        .collect();

    // One deploy call covers every writable file; run it once, up front, so each
    // step below reports the outcome that actually happened on disk.
    let applied = match (mode, writable.is_empty()) {
        (RepairMode::Apply, false) => Some(deploy_output_styles(claude)),
        _ => None,
    };

    drift
        .iter()
        .map(|(file_name, state)| {
            let path = styles_dir.join(file_name);
            // #7423: the tier label rides in so two tiers' `trusty-mpm.md` steps
            // are distinguishable in the preview.
            let what = match state {
                StyleDrift::Drifted => format!(
                    "redeploy `{file_name}` at the {tier} tier from the bundled output style \
                     (content drifted)"
                ),
                StyleDrift::Missing => format!(
                    "write `{file_name}` at the {tier} tier from the bundled output style \
                     (currently absent)"
                ),
                StyleDrift::Unreadable(_) => {
                    format!("redeploy `{file_name}` at the {tier} tier")
                }
            };
            let status = match (state, &applied) {
                (StyleDrift::Unreadable(why), _) => StepStatus::Refused(format!(
                    "{why} — tm will not overwrite a file it could not read"
                )),
                (_, None) => StepStatus::Planned,
                (_, Some(Err(e))) => StepStatus::Failed(e.to_string()),
                (_, Some(Ok(result))) => {
                    if result.deployed.iter().any(|d| d == file_name) {
                        StepStatus::Applied { backup: None }
                    } else if let Some(why) = result.failure_reason(file_name) {
                        // #5866: this style's own reason, never a sibling's.
                        StepStatus::Failed(why.to_string())
                    } else {
                        // The deploy ran and left this file alone, which after a
                        // reported drift means something else rewrote it in the
                        // meantime. Report what happened, never what was planned.
                        StepStatus::Refused(
                            "already matched the bundled content by the time the write ran"
                                .to_string(),
                        )
                    }
                }
            };
            RepairStep {
                check: "output_style_staleness",
                path,
                what,
                status,
            }
        })
        .collect()
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
            .filter(|e| e.file_name().to_str().is_some_and(|n| n.starts_with("tm-")))
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
