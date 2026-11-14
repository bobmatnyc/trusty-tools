//! `tm session delete --project-dir`: remove a session's project directory
//! behind destructive guards (#10992, epic #10987 phase 5).
//!
//! Why: owner ruling D8b — "delete = dry run + typed confirm + Touch ID".
//! Until now `tm session delete` only marked the record deleted
//! (`super::delete`); a project directory could be removed only by hand. A
//! removal is irreversible, so every guard fails closed: a probe that cannot
//! answer refuses, and no refusal is downgraded to a warning.
//! What: [`run_dir_delete`] is the one path for the dry run and the real run.
//! [`plan_dir_delete`] runs every check with no side effect; a dry run returns
//! its plan. The real run asks the caller to confirm, then
//! [`DirDeleteCapability::acquire`] — the only way to obtain something that
//! can remove a directory, modelled on `index_delete_guard`'s
//! `DestructiveIndexDelete::acquire` — checks the confirmation, asks the
//! [`Authenticator`] for presence, and re-runs the disk guards before
//! [`DirDeleteCapability::remove`] runs `remove_dir_all`.
//! Guards, in order: the directory is strictly inside the allowlisted root
//! (no symlink leaf; `..` and symlinked parents resolved by canonicalizing);
//! it holds no recorded Architect directory; no session working in it is the
//! Architect's or of an unreported kind; no session in it is running; it has
//! no dirty or unpushed work (`worktree_safety::inspect_dirt_excusing`, which
//! excuses only what `decommission --force` excuses: `is_provisioning_entry`,
//! #7660); a main checkout's `.git` holds no unpushed commit on any local
//! branch and no stash (`work_outside_head`); no gitignored file that is not
//! build output (`ignored_output_refusal`, #8534); no live
//! process stands in it (`worktree_liveness::process_holding`). That probe
//! sees each process's current working directory only, not open files: a
//! process holding a file inside the tree with its cwd elsewhere is not seen.
//! Test: `project_dir_delete_tests.rs`.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use thiserror::Error;

use super::architect::{ProtectedVerb, refusal};
use super::decommission_force::{WorktreeKind, is_provisioning_entry, worktree_kind};
use super::record::ManagedSessionState;
use super::session_kind::SessionKind;
use super::workspace_guard::is_safe_to_remove;
use super::worktree_ignored_output::ignored_output_refusal;
use super::worktree_safety::{count_unlanded, git_stdout};
use crate::client::ManagedSessionSummary;
use crate::core::presence_local::{Authenticator, PresenceOutcome};

/// Why a project-directory delete was refused or failed (#10992).
#[derive(Debug, Error)]
pub enum DirDeleteError {
    /// No session with this id was listed.
    #[error("session {0} not found")]
    SessionNotFound(String),
    /// The session records neither a workspace nor a working directory.
    #[error("session {0} records no project directory")]
    NoDirectory(String),
    /// The directory is not strictly inside the allowlisted root.
    #[error("refusing to delete {}: {reason} (allowlisted root {})", path.display(), root.display())]
    Uncontained {
        /// The directory as named.
        path: PathBuf,
        /// The allowlisted root.
        root: PathBuf,
        /// Why it is not contained.
        reason: String,
    },
    /// The directory is, or holds, a recorded Architect directory.
    #[error("refusing to delete {}: it holds the Architect's directory {}", path.display(), architect.display())]
    ArchitectDir {
        /// The directory.
        path: PathBuf,
        /// The Architect directory inside it.
        architect: PathBuf,
    },
    /// A session working in the directory is the Architect's, or of a kind
    /// the daemon did not report.
    #[error("{0}")]
    Protected(String),
    /// Sessions still run in the directory.
    #[error("refusing to delete {}: session(s) still running there: [{}]; stop them first", path.display(), running.join(", "))]
    Running {
        /// The directory.
        path: PathBuf,
        /// Names of the running sessions.
        running: Vec<String>,
    },
    /// The directory holds dirty or unpushed work, or the check could not run.
    #[error("refusing to delete {}: {reason}", path.display())]
    Dirty {
        /// The directory.
        path: PathBuf,
        /// What `inspect_dirt` found.
        reason: String,
    },
    /// A live process stands in the directory, or the probe could not run.
    #[error("refusing to delete {}: {reason}", path.display())]
    LiveProcess {
        /// The directory.
        path: PathBuf,
        /// What the liveness probe found.
        reason: String,
    },
    /// The typed confirmation did not match.
    #[error("not confirmed: typed {typed:?}, expected {expected:?}; nothing was deleted")]
    NotConfirmed {
        /// The name the caller had to type.
        expected: String,
        /// What was typed.
        typed: String,
    },
    /// Touch ID / password presence was declined or unavailable.
    #[error("presence not proven: {0}; nothing was deleted")]
    Presence(String),
    /// A test process asked to delete a directory outside the temp dir.
    #[error("refusing to delete {} from a test process: it is outside the temp dir", .0.display())]
    TestHarness(PathBuf),
    /// `remove_dir_all` failed; part of the tree may be gone.
    // #10992 critic MEDIUM: say the tree is partial, that a retry is refused
    // once `.git` is gone, and that the record was left alone.
    #[error(
        "deleting {} failed part-way ({error}): the directory is PARTIALLY deleted. \
         Remove the rest by hand: a retry is refused once `.git` is gone, because \
         the leftover then reads as unsaved work. The session record was NOT \
         marked deleted.",
        path.display()
    )]
    Remove {
        /// The directory.
        path: PathBuf,
        /// The filesystem error.
        error: std::io::Error,
    },
}

/// One session, as the guards need it (#10992).
#[derive(Debug, Clone, Default)]
pub struct DirSession {
    /// Session id.
    pub id: String,
    /// tmux name.
    pub name: String,
    /// Kind; `None` when the daemon did not report one (refused).
    pub kind: Option<SessionKind>,
    /// Every state token reported (display and persisted).
    pub states: Vec<String>,
    /// Workspace path, if any.
    pub workspace: Option<PathBuf>,
    /// Start directory, if any.
    pub cwd: Option<PathBuf>,
    /// The record's task, which an unedited `TASK.md` equals (#8688).
    pub task: Option<String>,
}

impl DirSession {
    /// Build from the daemon's list/get summary.
    pub fn from_summary(s: &ManagedSessionSummary) -> Self {
        let mut states = vec![s.state.clone()];
        states.extend(s.persisted_state.clone());
        Self {
            id: s.id.clone(),
            name: s.name.clone(),
            kind: s.kind,
            states,
            workspace: s.workspace_path.as_deref().map(PathBuf::from),
            cwd: s.cwd.as_deref().map(PathBuf::from),
            task: s.task.clone(),
        }
    }

    /// The project directory: the workspace, else the start directory.
    fn dir(&self) -> Option<&Path> {
        self.workspace.as_deref().or(self.cwd.as_deref())
    }

    /// Whether any reported state may be running. An unparsable token counts
    /// as running (fail closed).
    fn may_be_live(&self) -> bool {
        self.states
            .iter()
            .any(|t| match ManagedSessionState::from_wire(t) {
                Some(s) => matches!(
                    s,
                    ManagedSessionState::Active | ManagedSessionState::Provisioning
                ),
                None => true,
            })
    }

    /// Whether the workspace or start directory lies inside `canon`.
    fn works_inside(&self, canon: &Path) -> bool {
        [self.workspace.as_deref(), self.cwd.as_deref()]
            .into_iter()
            .flatten()
            .any(|p| {
                p.starts_with(canon) || fs::canonicalize(p).is_ok_and(|c| c.starts_with(canon))
            })
    }
}

/// The OS questions the guards ask, injectable for tests (#10992).
pub trait DirProbes {
    /// `Some(reason)` unless the directory provably holds no unsaved work.
    /// `task` is the session record's task, for the `TASK.md` excuse.
    fn dirt(&self, dir: &Path, task: Option<&str>) -> Option<String>;
    /// `Some(reason)` unless the probe ran and found no process in `dir`.
    fn live_process(&self, dir: &Path) -> Option<String>;
}

/// Production probes: `inspect_dirt_excusing` and `process_holding`.
#[derive(Debug, Clone, Copy, Default)]
pub struct OsProbes;

impl DirProbes for OsProbes {
    /// Why: a freshly spawned project dir holds tm's own provisioning files,
    /// which a plain dirt check counts as unsaved work.
    /// What: the dirt check `decommission --force` runs, excusing exactly
    /// [`is_provisioning_entry`] — the same function, so the two sets cannot
    /// drift; any other entry, or a check that cannot complete, is dirt.
    /// Test: `a_spawn_provisioned_dir_is_excused_every_provisioning_file`,
    /// `an_untracked_non_provisioning_file_still_refuses`.
    fn dirt(&self, dir: &Path, task: Option<&str>) -> Option<String> {
        // #10992 critic 834ddb80b2c5: the #7660 exemption, shared, never copied.
        let excuse = |line: &str| is_provisioning_entry(dir, task, line);
        super::worktree_safety::inspect_dirt_excusing(dir, &excuse).map(|d| d.reason)
    }

    fn live_process(&self, dir: &Path) -> Option<String> {
        super::worktree_liveness::process_holding(dir)
    }
}

/// What the caller answered when asked to confirm (#10992).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Confirmation {
    /// `--yes`.
    Yes,
    /// The text typed at the prompt.
    Typed(String),
}

/// The switches every delete takes (#10992).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DirDeleteOptions {
    /// Report the plan; change nothing.
    pub dry_run: bool,
}

/// A checked delete: what would be removed (#10992).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DirDeletePlan {
    /// The session.
    pub session_id: String,
    /// The directory, canonical.
    pub dir: PathBuf,
    /// The allowlisted root, canonical.
    pub root: PathBuf,
    /// The name a typed confirmation must match: the directory's leaf name.
    pub confirm_name: String,
    /// The session record's task, re-read by the guards in `acquire`.
    #[serde(skip)]
    pub task: Option<String>,
}

/// What a delete did, or would do under `dry_run` (#10992).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DirDeleteReport {
    /// Whether this was a dry run.
    pub dry_run: bool,
    /// The plan.
    pub plan: DirDeletePlan,
    /// Whether the directory was removed.
    pub removed: bool,
}

/// The inputs every delete reads (#10992).
#[derive(Debug, Clone, Copy)]
pub struct DirDeleteInput<'a> {
    /// The session whose directory goes.
    pub session_id: &'a str,
    /// Every session the daemon lists.
    pub sessions: &'a [DirSession],
    /// The allowlisted root (the managed workspace root).
    pub root: &'a Path,
    /// Recorded Architect directories, never deleted.
    pub architect_dirs: &'a [PathBuf],
}

fn uncontained(path: &Path, root: &Path, reason: impl Into<String>) -> DirDeleteError {
    DirDeleteError::Uncontained {
        path: path.to_path_buf(),
        root: root.to_path_buf(),
        reason: reason.into(),
    }
}

/// The canonical `dir` when it is a real directory strictly inside `root`.
///
/// Why: a symlink leaf would make the named path and the removed tree differ;
/// `..` segments and symlinked parents are resolved by canonicalizing before
/// the prefix test. What: refuses a symlink leaf, a non-directory, and
/// anything `is_safe_to_remove` rejects (outside root, the root itself, home,
/// shallow paths, unresolvable paths).
/// Test: `a_path_outside_the_root_is_never_removed`,
/// `a_dotdot_escape_is_refused`, `a_symlink_leaf_is_refused`,
/// `a_symlinked_parent_escape_is_refused`, `the_root_itself_is_refused`.
pub fn contained_dir(dir: &Path, root: &Path) -> Result<PathBuf, DirDeleteError> {
    let meta = fs::symlink_metadata(dir)
        .map_err(|e| uncontained(dir, root, format!("cannot stat it: {e}")))?;
    if meta.file_type().is_symlink() {
        return Err(uncontained(dir, root, "it is a symlink"));
    }
    if !meta.is_dir() {
        return Err(uncontained(dir, root, "it is not a directory"));
    }
    if !is_safe_to_remove(dir, root) {
        return Err(uncontained(
            dir,
            root,
            "it is not strictly inside the allowlisted root",
        ));
    }
    fs::canonicalize(dir).map_err(|e| uncontained(dir, root, format!("cannot resolve it: {e}")))
}

/// Why `dir`'s own repository holds work outside `HEAD`, or `None` (#10992).
///
/// Why: in a main checkout `.git` sits inside the removed tree, so every
/// local branch and the stash go with it, and the dirt check counts only
/// `HEAD` and `session/<leaf>`. What: for a main checkout
/// ([`worktree_kind`]), the commits on any local branch reachable from no
/// remote, less those whose patch already landed ([`count_unlanded`],
/// #6507), plus a non-empty `refs/stash`. A linked worktree's branches live
/// in its common dir outside the tree; a directory with no `.git` has none.
/// Every probe failure refuses.
/// Test: `an_unpushed_commit_on_another_branch_refuses`, `a_stash_refuses`,
/// `a_failed_branch_probe_refuses`.
fn work_outside_head(dir: &Path) -> Option<String> {
    let failed = |e: String| Some(format!("the branch and stash check failed: {e}"));
    match fs::symlink_metadata(dir.join(".git")) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => return failed(format!("cannot stat `.git`: {e}")),
        Ok(_) => {}
    }
    match worktree_kind(dir) {
        Ok(WorktreeKind::Linked(_)) => return None,
        Ok(WorktreeKind::MainCheckout) => {}
        Err(e) => return failed(e),
    }
    let probe = || -> Result<(usize, bool), String> {
        let heads = git_stdout(dir, &["for-each-ref", "--format=%(refname)", "refs/heads"])?;
        let tips: Vec<&str> = heads.split_whitespace().collect();
        let unpushed = if tips.is_empty() {
            0
        } else {
            let count = ["rev-list", "--count", "--branches", "--not", "--remotes"];
            let list = ["rev-list", "--branches", "--not", "--remotes"];
            count_unlanded(dir, &count, &list, &tips)?
        };
        let stash = git_stdout(dir, &["for-each-ref", "--format=%(refname)", "refs/stash"])?;
        Ok((unpushed, !stash.trim().is_empty()))
    };
    match probe() {
        Err(e) => failed(e),
        Ok((0, false)) => None,
        Ok((unpushed, stash)) => Some(format!(
            "its `.git` holds {unpushed} unpushed commit(s) on local branches and {} stash; \
             deleting the directory deletes them",
            if stash { "a" } else { "no" }
        )),
    }
}

/// The disk guards, run at plan time and again just before removal.
fn disk_guards(
    dir: &Path,
    task: Option<&str>,
    probes: &dyn DirProbes,
) -> Result<(), DirDeleteError> {
    // #10992: an unanswerable dirt check is dirt, never clean. The two checks
    // after it (critic HIGH 1, HIGH 2) refuse the same way.
    let dirt = probes
        .dirt(dir, task)
        .or_else(|| work_outside_head(dir))
        .or_else(|| ignored_output_refusal(dir));
    if let Some(reason) = dirt {
        return Err(DirDeleteError::Dirty {
            path: dir.to_path_buf(),
            reason,
        });
    }
    // #10992: an unanswerable liveness probe is a live process, never none.
    if let Some(reason) = probes.live_process(dir) {
        return Err(DirDeleteError::LiveProcess {
            path: dir.to_path_buf(),
            reason,
        });
    }
    Ok(())
}

/// Every check, with no side effect (#10992).
///
/// What: see the module doc for the order; each refusal returns before the
/// next check runs.
/// Test: `dry_run_removes_nothing_and_reports_the_same_plan`, and one
/// refusal test per guard in `project_dir_delete_tests.rs`.
pub fn plan_dir_delete(
    input: DirDeleteInput<'_>,
    probes: &dyn DirProbes,
) -> Result<DirDeletePlan, DirDeleteError> {
    let target = input
        .sessions
        .iter()
        .find(|s| s.id == input.session_id)
        .ok_or_else(|| DirDeleteError::SessionNotFound(input.session_id.to_string()))?;
    let dir = target
        .dir()
        .ok_or_else(|| DirDeleteError::NoDirectory(target.id.clone()))?;
    let canon = contained_dir(dir, input.root)?;
    let root =
        fs::canonicalize(input.root).map_err(|e| uncontained(dir, input.root, e.to_string()))?;
    if let Some(a) = input.architect_dirs.iter().find(|a| {
        let ca = fs::canonicalize(a).unwrap_or_else(|_| a.to_path_buf());
        ca.starts_with(&canon) || a.starts_with(&canon)
    }) {
        return Err(DirDeleteError::ArchitectDir {
            path: canon,
            architect: a.clone(),
        });
    }
    let occupants: Vec<&DirSession> = input
        .sessions
        .iter()
        .filter(|s| s.id == target.id || s.works_inside(&canon))
        .collect();
    // #10992: the existing ProtectedVerb refusal; an unreported kind reads as
    // `Unknown`, which is protected too.
    if let Some(s) = occupants
        .iter()
        .find(|s| s.kind.is_none_or(SessionKind::is_protected))
    {
        let kind = s.kind.unwrap_or(SessionKind::Unknown);
        return Err(DirDeleteError::Protected(refusal(
            &s.name,
            kind,
            ProtectedVerb::Delete,
        )));
    }
    let running: Vec<String> = occupants
        .iter()
        .filter(|s| s.may_be_live())
        .map(|s| s.name.clone())
        .collect();
    if !running.is_empty() {
        return Err(DirDeleteError::Running {
            path: canon,
            running,
        });
    }
    disk_guards(&canon, target.task.as_deref(), probes)?;
    let confirm_name = canon
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(DirDeletePlan {
        session_id: target.id.clone(),
        dir: canon,
        root,
        confirm_name,
        task: target.task.clone(),
    })
}

/// The capability to remove one checked project directory (#10992).
///
/// Why: modelled on `DestructiveIndexDelete` — the private field and the
/// single constructor make an unconfirmed, unproven or unguarded removal
/// unrepresentable. What: holds the canonical directory.
/// Test: `acquire_refuses_a_mismatched_typed_name`,
/// `acquire_refuses_without_presence`,
/// `acquire_rechecks_the_disk_guards_after_presence`.
#[derive(Debug)]
pub struct DirDeleteCapability {
    dir: PathBuf,
}

impl DirDeleteCapability {
    /// Acquire the capability for `plan`, or say why not.
    ///
    /// What, each step failing closed: the typed name must equal
    /// `plan.confirm_name` (`--yes` skips it); `auth` must answer `Granted`;
    /// the directory must still be contained; a test process may only remove
    /// inside the temp dir; and the disk guards run once more, after the
    /// prompt, so work made while the user typed still refuses.
    pub fn acquire(
        plan: &DirDeletePlan,
        confirm: &Confirmation,
        auth: &dyn Authenticator,
        probes: &dyn DirProbes,
    ) -> Result<Self, DirDeleteError> {
        if let Confirmation::Typed(typed) = confirm
            && typed.trim() != plan.confirm_name
        {
            return Err(DirDeleteError::NotConfirmed {
                expected: plan.confirm_name.clone(),
                typed: typed.trim().to_string(),
            });
        }
        let reason = format!(
            "delete the project directory {} of tm session {}",
            plan.dir.display(),
            plan.session_id
        );
        match auth.authenticate(&reason) {
            PresenceOutcome::Granted => {}
            PresenceOutcome::Declined(why) => return Err(DirDeleteError::Presence(why)),
            PresenceOutcome::Unavailable(why) => return Err(DirDeleteError::Presence(why)),
        }
        let dir = contained_dir(&plan.dir, &plan.root)?;
        if trusty_common::running_under_test_harness() {
            let tmp =
                fs::canonicalize(std::env::temp_dir()).unwrap_or_else(|_| std::env::temp_dir());
            if !dir.starts_with(&tmp) {
                return Err(DirDeleteError::TestHarness(dir));
            }
        }
        disk_guards(&dir, plan.task.as_deref(), probes)?;
        Ok(Self { dir })
    }

    /// Remove the directory tree; returns the path removed.
    ///
    /// Test: `a_partial_delete_says_so_and_names_the_record`.
    pub fn remove(self) -> Result<PathBuf, DirDeleteError> {
        fs::remove_dir_all(&self.dir).map_err(|error| DirDeleteError::Remove {
            path: self.dir.clone(),
            error,
        })?;
        tracing::info!(dir = %self.dir.display(), "project directory deleted (#10992)");
        Ok(self.dir)
    }
}

/// The one path for the dry run and the real run (#10992).
///
/// What: [`plan_dir_delete`]; a dry run returns its plan. Otherwise `confirm`
/// is shown the plan and answers, then [`DirDeleteCapability::acquire`] and
/// [`DirDeleteCapability::remove`].
/// Test: `dry_run_removes_nothing_and_reports_the_same_plan`,
/// `a_confirmed_present_delete_removes_a_clean_pushed_checkout`.
pub fn run_dir_delete(
    input: DirDeleteInput<'_>,
    opts: DirDeleteOptions,
    confirm: &mut dyn FnMut(&DirDeletePlan) -> Confirmation,
    auth: &dyn Authenticator,
    probes: &dyn DirProbes,
) -> Result<DirDeleteReport, DirDeleteError> {
    let plan = plan_dir_delete(input, probes)?;
    if opts.dry_run {
        return Ok(DirDeleteReport {
            dry_run: true,
            plan,
            removed: false,
        });
    }
    let answer = confirm(&plan);
    DirDeleteCapability::acquire(&plan, &answer, auth, probes)?.remove()?;
    Ok(DirDeleteReport {
        dry_run: false,
        plan,
        removed: true,
    })
}

#[cfg(all(test, unix))]
#[path = "project_dir_delete_tests.rs"]
mod tests;
