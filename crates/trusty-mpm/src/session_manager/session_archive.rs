//! `tm session archive`: move a session's project directory to the archive
//! (#10991).
//!
//! Why: owner ruling D8b — archive moves the directory to the 0700 archive
//! root, recording where it came from. A session still running in that
//! directory would lose its files mid-run, so it refuses unless forced, and a
//! forced archive stops those sessions before anything moves.
//! What: [`SessionManager::archive_session`] resolves the directory, checks the
//! sessions that run in it, plans the move ([`super::archive_move::plan_archive`]),
//! and — unless `dry_run` — stops them, executes the plan off the async
//! runtime, and stamps the project row archived. The dry run is the same
//! function returning before the first side effect.
//! Test: `session_archive_tests.rs`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::info;

use super::architect::{ProtectedVerb, refusal, refuses_verbs};
use super::archive_move::{self, ArchiveManifest, ArchiveMoveError, ArchivePlan};
use super::manager::{ManagedError, SessionManager};
use super::record::{ManagedSessionId, ManagedSessionState, SessionRecord};
use crate::project::{Project, ProjectRegistry, ProjectStoreError, resolve_session_project};

/// The two switches every archive entry point takes (#10991).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ArchiveOptions {
    /// Report the plan; change nothing.
    pub dry_run: bool,
    /// Stop the sessions running in the directory instead of refusing.
    pub force: bool,
}

/// Why a session archive did not complete (#10991).
#[derive(Debug, Error)]
pub enum SessionArchiveError {
    /// The session is unknown, or a session-store call failed.
    #[error(transparent)]
    Session(#[from] ManagedError),
    /// A session in the directory is the Architect's; nothing was stopped.
    #[error("{0}")]
    Protected(String),
    /// Sessions still run in the directory and `force` was not given.
    #[error(
        "refusing to archive {}: session(s) still running there: [{}]; \
         stop them first or pass --force",
        path.display(),
        running.join(", ")
    )]
    Running {
        /// The directory.
        path: PathBuf,
        /// Ids of the running sessions.
        running: Vec<String>,
    },
    /// A forced stop failed; nothing was moved.
    #[error(
        "archive not started: {} session(s) failed to stop ({}); stopped: [{}]",
        failed.len(),
        failed.iter().map(|(id, e)| format!("{id}: {e}")).collect::<Vec<_>>().join("; "),
        stopped.join(", ")
    )]
    StopFailed {
        /// Ids that did stop.
        stopped: Vec<String>,
        /// `(id, error)` for each that did not.
        failed: Vec<(String, String)>,
    },
    /// The registry could not be read; nothing was moved.
    #[error("project registry: {0}")]
    Registry(ProjectStoreError),
    /// The plan or the move failed; see the move error for what is on disk.
    #[error(transparent)]
    Move(#[from] ArchiveMoveError),
    /// The directory is archived, but its project row was not stamped.
    #[error(
        "archived to {}, but project '{project}' could not be marked archived: {error}",
        entry.display()
    )]
    MarkFailed {
        /// The complete archive entry.
        entry: PathBuf,
        /// The project name.
        project: String,
        /// The registry error.
        error: ProjectStoreError,
    },
    /// The blocking move task did not finish.
    #[error("archive task failed: {0}")]
    Join(String),
}

/// What an archive did, or would do under `dry_run` (#10991).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionArchiveReport {
    /// Whether this was a dry run (nothing changed).
    pub dry_run: bool,
    /// The move: source, archive entry, destination and manifest path.
    pub plan: ArchivePlan,
    /// Ids of the sessions running in the directory, stopped (or, in a dry
    /// run, to be stopped) because of `force`.
    pub stopped: Vec<String>,
    /// The manifest written; `None` in a dry run.
    pub manifest: Option<ArchiveManifest>,
    /// Whether the project row is (or, in a dry run, would be) stamped
    /// archived; see [`SessionManager::prepare_archive`] (#10991).
    #[serde(default)]
    pub project_archived: bool,
}

/// A checked, planned archive, ready for [`SessionManager::finish_archive`].
///
/// Why: #10991 critic round — the daemon runs the finish detached from the
/// HTTP request, so a caller's timeout cannot drop the project stamp, and it
/// needs the plan (the entry path) before the move starts.
#[derive(Debug, Clone)]
pub struct PreparedArchive {
    /// The move.
    pub plan: ArchivePlan,
    /// Sessions running in the directory, to stop before the move (`force`).
    pub running: Vec<ManagedSessionId>,
    /// The project to stamp archived after the move, if any.
    pub mark_project: Option<String>,
}

impl PreparedArchive {
    /// The report a dry run returns: the plan, who would stop, and whether the
    /// project would be stamped.
    pub fn into_dry_run_report(self) -> SessionArchiveReport {
        SessionArchiveReport {
            dry_run: true,
            stopped: self.running.iter().map(ToString::to_string).collect(),
            project_archived: self.mark_project.is_some(),
            plan: self.plan,
            manifest: None,
        }
    }
}

/// The directory a record works in: its workspace, else its start directory.
fn session_dir(r: &SessionRecord) -> &Path {
    r.workspace_path.as_deref().unwrap_or(&r.cwd)
}

/// Whether `r` works inside `dir` (lexically, or once both are canonical).
fn works_inside(r: &SessionRecord, dir: &Path, canon: &Path) -> bool {
    let d = session_dir(r);
    d.starts_with(dir) || std::fs::canonicalize(d).is_ok_and(|c| c.starts_with(canon))
}

/// Whether `project` names `canon` as its own checkout: a local-path
/// `repo_url` that resolves to the moved directory.
fn is_own_checkout(project: &Project, canon: &Path) -> bool {
    Path::new(&project.repo_url).is_absolute()
        && std::fs::canonicalize(&project.repo_url).is_ok_and(|c| c == canon)
}

impl SessionManager {
    /// Archive session `id`'s project directory under `root`.
    ///
    /// Why: see the module doc. What: [`Self::prepare_archive`]; a dry run
    /// reports the plan; otherwise [`Self::finish_archive`]. Session records
    /// are kept.
    /// Test: `archive_session_moves_the_directory_and_marks_the_project`,
    /// `archive_dry_run_changes_nothing`,
    /// `archive_refuses_a_running_session_without_force`,
    /// `archive_force_stops_the_session_first`,
    /// `archive_refuses_an_architect_session`,
    /// `archive_failed_stop_moves_nothing`,
    /// `archive_unreadable_registry_moves_nothing`,
    /// `archive_reports_a_failed_project_stamp`,
    /// `archiving_one_worktree_session_leaves_a_live_project_unarchived`.
    pub async fn archive_session(
        &self,
        registry: &ProjectRegistry,
        id: &ManagedSessionId,
        opts: ArchiveOptions,
        root: &Path,
    ) -> Result<SessionArchiveReport, SessionArchiveError> {
        let prepared = self.prepare_archive(registry, id, opts, root).await?;
        if opts.dry_run {
            return Ok(prepared.into_dry_run_report());
        }
        self.finish_archive(registry, prepared).await
    }

    /// Every check and the plan, with no side effect (#10991).
    ///
    /// What, each step failing closed before the next: look up the record;
    /// refuse when any non-terminal session working inside the directory is
    /// the Architect's ([`SessionArchiveError::Protected`]); refuse when any
    /// of them is `Active`/`Provisioning` and `force` is off
    /// ([`SessionArchiveError::Running`]); read the registry; plan the move.
    /// The project is to be stamped archived only when the moved directory
    /// is its own checkout, or when no other non-terminal session outside
    /// the directory resolves to it — archiving one worktree must not hide a
    /// project that still runs elsewhere.
    /// Test: `archiving_one_worktree_session_leaves_a_live_project_unarchived`.
    pub async fn prepare_archive(
        &self,
        registry: &ProjectRegistry,
        id: &ManagedSessionId,
        opts: ArchiveOptions,
        root: &Path,
    ) -> Result<PreparedArchive, SessionArchiveError> {
        let record = self.get(id).await?;
        let dir = session_dir(&record).to_path_buf();
        let canon = std::fs::canonicalize(&dir).unwrap_or_else(|_| dir.clone());
        let all = self.list().await;
        let occupants: Vec<&SessionRecord> = all
            .iter()
            .filter(|r| !r.state.is_terminal() || r.id == record.id)
            .filter(|r| works_inside(r, &dir, &canon))
            .collect();
        // #8942: the Architect's directory is never moved, `force` or not.
        if let Some(r) = occupants.iter().find(|r| refuses_verbs(r.kind, &r.state)) {
            return Err(SessionArchiveError::Protected(refusal(
                &r.tmux_name,
                r.kind,
                ProtectedVerb::Archive,
            )));
        }
        let running: Vec<ManagedSessionId> = occupants
            .iter()
            .filter(|r| {
                matches!(
                    r.state,
                    ManagedSessionState::Active | ManagedSessionState::Provisioning
                )
            })
            .map(|r| r.id)
            .collect();
        if !running.is_empty() && !opts.force {
            return Err(SessionArchiveError::Running {
                path: dir,
                running: running.iter().map(ToString::to_string).collect(),
            });
        }
        let projects = registry
            .list()
            .await
            .map_err(SessionArchiveError::Registry)?;
        let project = resolve_session_project(&record, &projects);
        // #10991 critic HIGH: a worktree of a project that runs elsewhere is
        // archived without hiding the project.
        let mark_project = project
            .filter(|p| {
                let only = std::slice::from_ref(*p);
                is_own_checkout(p, &canon)
                    || !all.iter().any(|r| {
                        r.id != record.id
                            && !r.state.is_terminal()
                            && !works_inside(r, &dir, &canon)
                            && resolve_session_project(r, only).is_some()
                    })
            })
            .map(|p| p.name.clone());
        let plan = archive_move::plan_archive(
            &dir,
            root,
            &record.id.to_string(),
            project.map(|p| p.name.clone()),
            chrono::Utc::now(),
        )?;
        Ok(PreparedArchive {
            plan,
            running,
            mark_project,
        })
    }

    /// Stop, move and stamp a [`PreparedArchive`] (#10991).
    ///
    /// What: stops the running sessions (any failure is
    /// [`SessionArchiveError::StopFailed`] with nothing moved), executes the
    /// plan on a blocking thread, then stamps `mark_project` archived (a
    /// failure is [`SessionArchiveError::MarkFailed`], naming the entry).
    /// Test: `archive_failed_stop_moves_nothing`,
    /// `archive_reports_a_failed_project_stamp`.
    pub async fn finish_archive(
        &self,
        registry: &ProjectRegistry,
        prepared: PreparedArchive,
    ) -> Result<SessionArchiveReport, SessionArchiveError> {
        let PreparedArchive {
            plan,
            running,
            mark_project,
        } = prepared;
        let (mut stopped, mut failed) = (Vec::new(), Vec::new());
        for sid in &running {
            match self.stop(sid).await {
                Ok(_) => stopped.push(sid.to_string()),
                Err(e) => failed.push((sid.to_string(), e.to_string())),
            }
        }
        // #10991 fail-open check: a session that did not stop keeps its files.
        if !failed.is_empty() {
            return Err(SessionArchiveError::StopFailed { stopped, failed });
        }
        let to_run = plan.clone();
        let manifest = tokio::task::spawn_blocking(move || archive_move::execute_plan(&to_run))
            .await
            .map_err(|e| SessionArchiveError::Join(e.to_string()))??;
        if let Some(name) = &mark_project {
            registry
                .mark_archived(name, manifest.archived_at)
                .await
                .map_err(|error| SessionArchiveError::MarkFailed {
                    entry: plan.entry.clone(),
                    project: name.clone(),
                    error,
                })?;
        }
        info!(session = %plan.session_id, entry = %plan.entry.display(), "session directory archived (#10991)");
        Ok(SessionArchiveReport {
            dry_run: false,
            plan,
            stopped,
            manifest: Some(manifest),
            project_archived: mark_project.is_some(),
        })
    }
}

#[cfg(all(test, unix))]
#[path = "session_archive_tests.rs"]
mod tests;
