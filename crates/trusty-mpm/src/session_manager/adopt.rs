//! Explicit adoption of an EXISTING, unmanaged tmux session (#1433).
//!
//! Why: the session-manager normally only drives sessions it `create`d, but an
//! operator often already has a live tmux pane (a hand-started Claude Code, a
//! session created outside trusty-mpm, or one whose record was lost) that they
//! want to connect to and drive through the full managed surface. This is the
//! EXPLICIT counterpart to [`SessionManager::reconcile_on_boot`]'s automatic
//! adoption. It lives in its own module so [`manager`](super::manager) stays under
//! the 500-SLOC production cap.
//! What: an inherent `impl SessionManager` block adding
//! [`SessionManager::adopt_existing`]; a free helper
//! [`derive_source_id_for_record`] used by the reconcile backfill (#1780).
//! Test: `manager_adopt_existing_*` in `super::tests`;
//! `derive_source_id_*` unit tests at the bottom of this module.

use std::path::{Path, PathBuf};

use chrono::Utc;
use tracing::info;

use super::manager::{ManagedError, SessionManager};
use super::record::{ManagedSessionId, ManagedSessionState, SessionRecord};

/// Derive the `source_id` (`"owner/repo"`) for a session record from its workspace git remote.
///
/// Why (#1780): the reconcile backfill needs to populate `source_id` for records
/// that were stored before the field existed or that were auto-adopted without one.
/// Centralising the derivation here keeps `manager.rs` under its 500-SLOC cap and
/// makes the logic independently unit-testable.
/// What: tries `workspace_path` first (if the directory exists); falls back to
/// `cwd` when it is not the sentinel `/unknown` and still exists on disk.
/// Calls `trusty_common::github_path::derive_github_path`, which shells to
/// `git config --get remote.origin.url` — no network. Returns `None` when no
/// usable path is available, the directory is not a git repo, or the remote URL
/// is not parseable as a `owner/repo` identity; callers skip gracefully in that case.
/// Test: `derive_source_id_ssh_remote`, `derive_source_id_https_remote`,
/// `derive_source_id_none_for_no_remote`, `derive_source_id_none_for_unknown_path`.
pub(super) fn derive_source_id_for_record(record: &SessionRecord) -> Option<String> {
    let path: &Path = record
        .workspace_path
        .as_deref()
        .filter(|p| p.is_dir())
        .or_else(|| {
            let c = record.cwd.as_path();
            (c != Path::new("/unknown") && c.is_dir()).then_some(c)
        })?;
    let gh = trusty_common::github_path::derive_github_path(path)?;
    Some(format!("{}/{}", gh.owner, gh.repo))
}

/// Batch-derive `source_id` for all records missing it in one `spawn_blocking`.
///
/// Why (#1780): shelling to `git config --get remote.origin.url` inside an
/// async loop would block the tokio executor for every null-source_id record
/// at daemon boot time (136 sessions observed in the fleet). A single
/// `spawn_blocking` call batches all N derivations on one thread-pool thread,
/// freeing the async executor immediately.
/// What: collects `(index, path)` pairs for non-Decommissioned records with
/// `source_id == None`, derives all in one `spawn_blocking`, then writes
/// results back in-place by index. Records where derivation fails (no git,
/// no remote, non-GitHub URL, `/unknown`) are left unchanged — one bad record
/// never aborts the pass.
/// Test: `backfill_tests::reconcile_backfills_source_id_from_workspace_git_remote`
/// (Active path) and `backfill_tests::reconcile_backfills_source_id_for_stopped_record`
/// (Stopped path).
pub(super) async fn backfill_source_ids(records: &mut [SessionRecord]) {
    // Collect (record_index, cloned_record) for records that need derivation.
    // The clone is cheap (SessionRecord is a small owned-data struct) and lets
    // derive_source_id_for_record run inside the spawn_blocking closure.
    let to_derive: Vec<(usize, SessionRecord)> = records
        .iter()
        .enumerate()
        .filter(|(_, r)| {
            r.source_id.is_none() && !matches!(r.state, ManagedSessionState::Decommissioned)
        })
        .map(|(i, r)| (i, r.clone()))
        .collect();
    if to_derive.is_empty() {
        return;
    }
    let derived: Vec<(usize, Option<String>)> = tokio::task::spawn_blocking(move || {
        to_derive
            .into_iter()
            .map(|(idx, record)| (idx, derive_source_id_for_record(&record)))
            .collect()
    })
    .await
    .unwrap_or_default();
    for (idx, sid) in derived {
        if let Some(s) = sid {
            info!(name = %records[idx].tmux_name, source_id = %s, "reconcile: backfilled source_id");
            records[idx].source_id = Some(s);
        }
    }
}

impl SessionManager {
    /// Adopt an EXISTING, unmanaged tmux session into the durable store (#1433).
    ///
    /// Why: connect the managed surface to a live pane the operator already has so
    /// it can be driven through observe/send/stop/resume, rather than spawning a
    /// new one. This is the EXPLICIT counterpart to
    /// [`reconcile_on_boot`](SessionManager::reconcile_on_boot)'s automatic
    /// adoption — it synthesises a durable `Active` record for a pane that already
    /// exists.
    ///
    /// Design decisions (DOC-20 / DOC-14):
    /// - **Collision check is INVERTED vs. `create`.** `create` fails when the name
    ///   already exists; `adopt_existing` fails with
    ///   [`ManagedError::TmuxSessionMissing`] when the pane does NOT exist (you
    ///   cannot adopt what is not there) and with [`ManagedError::AlreadyAdopted`]
    ///   when the store already tracks the name.
    /// - **The already-adopted check and the upsert run under ONE held write lock.**
    ///   Reading the store to test for the name and then upserting must be atomic —
    ///   otherwise two concurrent `adopt_existing` calls for the same `tmux_name`
    ///   could both observe "absent" before either writes, producing duplicate
    ///   `Active` records (TOCTOU). We therefore take the store write guard ONCE,
    ///   scan the live records through it (`all()` reloads-on-read so a record
    ///   another process registered is also seen), reject a tracked name, and
    ///   upsert — all without releasing the guard. The tmux `session_exists`
    ///   liveness check stays OUTSIDE the lock (it touches tmux, not the store, and
    ///   a stale pane between the check and the lock is the operator's race to lose,
    ///   not a store-consistency hazard).
    /// - **No `create_session` call.** The pane already exists; the driver is only
    ///   consulted via `session_exists` to verify presence.
    /// - **`cwd` is REQUIRED** (a plain `PathBuf`): the pane's provenance is unknown
    ///   to the daemon, so the operator supplies the working directory rather than
    ///   have it stubbed to `/unknown` (as auto-boot adoption does). `task` may be
    ///   empty. `runtime` is caller-supplied; call sites that do not care pass
    ///   [`crate::runtime::RuntimeKind::default`].
    /// - **Non-managed names are ALLOWED.** Unlike `reconcile_on_boot` (which filters
    ///   to [`crate::core::names::is_managed_session_name`] — `tm-`/`tmpm-`/
    ///   `trusty-mpm-` — for SAFE automatic adoption), this explicit path
    ///   adopts ANY name the operator names. The reconcile filter is left untouched.
    ///
    /// What: verifies the pane exists, rejects an already-tracked name, then upserts
    /// an `Active` [`SessionRecord`] carrying the supplied `cwd`/`task`/`runtime`/
    /// `ephemeral` (a fresh id, no workspace/repo/branch — provenance is unknown).
    /// `ephemeral` (#1508) tags a throwaway adoption (e.g. an e2e test pane) so the
    /// auto-reap paths may decommission it; operator adoptions pass `false`.
    /// Returns the new record.
    /// Test: `manager_adopt_existing_registers_active` (also asserts the returned
    /// record's `cwd` matches the adopted cwd),
    /// `manager_adopt_existing_missing_tmux_errors`,
    /// `manager_adopt_existing_double_adopt_errors` (the single-locked path still
    /// rejects a second adopt of the same name),
    /// `manager_adopt_existing_allows_non_tmpm_name` in `super::tests`.
    pub async fn adopt_existing(
        &self,
        tmux_name: &str,
        cwd: PathBuf,
        task: String,
        runtime: crate::runtime::RuntimeKind,
        ephemeral: bool,
    ) -> Result<SessionRecord, ManagedError> {
        // The pane MUST already exist — adoption connects, it does not spawn.
        // `tmux_driver()` is the public accessor over the shared driver Arc. This
        // liveness check touches tmux (not the store), so it stays OUTSIDE the
        // store write lock taken below.
        if !self.tmux_driver().session_exists(tmux_name) {
            return Err(ManagedError::TmuxSessionMissing(tmux_name.to_string()));
        }

        let record = SessionRecord {
            id: ManagedSessionId::new(),
            tmux_name: tmux_name.to_string(),
            cwd,
            task,
            state: ManagedSessionState::Active,
            created_at: Utc::now(),
            last_activity_at: None,
            workspace_path: None,
            repo_url: None,
            branch: None,
            pending_decision: None,
            proposed_default: None,
            correlation: Default::default(),
            runtime,
            ephemeral,
            // Adopted sessions point at a pre-existing pane: the SM did NOT
            // create the workspace, so decommission must never delete it (#1511).
            workspace_owned: false,
            // Adopted sessions have no tracked source project.
            source_id: None,
            claude_session_id: None,
            scrollback_path: None,
            last_cwd: None,
        };

        // ── Atomic already-adopted check + upsert under ONE held write guard ──────
        // Acquire the store write lock ONCE and keep it for both the existence
        // scan and the upsert, so two concurrent adopts of the same `tmux_name`
        // cannot both pass the "absent" test and create duplicate `Active` records
        // (the TOCTOU the previous read-then-write split exposed). `all()` requires
        // `&mut self` and reloads-on-read, so a record another process registered
        // is also seen through this same guard.
        {
            let mut store = self.store.write().await;
            if store.all().await?.iter().any(|r| r.tmux_name == tmux_name) {
                return Err(ManagedError::AlreadyAdopted(tmux_name.to_string()));
            }
            store.upsert(record.clone()).await?;
        }
        info!(
            id = %record.id,
            name = %tmux_name,
            runtime = %runtime.as_str(),
            "adopted existing tmux session into the managed store"
        );
        Ok(record)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use std::process::Command;
    use tempfile::TempDir;

    fn make_record_with_workspace(workspace: PathBuf) -> SessionRecord {
        SessionRecord {
            id: ManagedSessionId::new(),
            tmux_name: "tmpm-test".into(),
            cwd: workspace.clone(),
            task: "test".into(),
            state: crate::session_manager::ManagedSessionState::Active,
            created_at: Utc::now(),
            last_activity_at: None,
            workspace_path: Some(workspace),
            repo_url: None,
            branch: None,
            pending_decision: None,
            proposed_default: None,
            correlation: Default::default(),
            runtime: Default::default(),
            ephemeral: false,
            workspace_owned: false,
            source_id: None,
            claude_session_id: None,
            scrollback_path: None,
            last_cwd: None,
        }
    }

    fn make_record_unknown_cwd() -> SessionRecord {
        SessionRecord {
            id: ManagedSessionId::new(),
            tmux_name: "tmpm-external".into(),
            cwd: PathBuf::from("/unknown"),
            task: "adopted session".into(),
            state: crate::session_manager::ManagedSessionState::Active,
            created_at: Utc::now(),
            last_activity_at: None,
            workspace_path: None,
            repo_url: None,
            branch: None,
            pending_decision: None,
            proposed_default: None,
            correlation: Default::default(),
            runtime: Default::default(),
            ephemeral: false,
            workspace_owned: false,
            source_id: None,
            claude_session_id: None,
            scrollback_path: None,
            last_cwd: None,
        }
    }

    /// Why: SSH scp-style remotes must yield a slugified owner/repo.
    /// Test: itself.
    #[test]
    fn derive_source_id_ssh_remote() {
        let tmp = TempDir::new().expect("tempdir");
        let dir = tmp.path();
        let git = |args: &[&str]| {
            Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(args)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        };
        if !git(&["init"]) {
            return; // no git on runner
        }
        git(&[
            "remote",
            "add",
            "origin",
            "git@github.com:test-owner/my-repo.git",
        ]);
        let record = make_record_with_workspace(dir.to_path_buf());
        let sid = derive_source_id_for_record(&record).expect("should derive");
        assert_eq!(sid, "test-owner/my-repo");
    }

    /// Why: HTTPS remotes must also yield a parseable owner/repo.
    /// Test: itself.
    #[test]
    fn derive_source_id_https_remote() {
        let tmp = TempDir::new().expect("tempdir");
        let dir = tmp.path();
        let git = |args: &[&str]| {
            Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(args)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        };
        if !git(&["init"]) {
            return;
        }
        git(&[
            "remote",
            "add",
            "origin",
            "https://github.com/acme-org/my-project.git",
        ]);
        let record = make_record_with_workspace(dir.to_path_buf());
        let sid = derive_source_id_for_record(&record).expect("should derive");
        assert_eq!(sid, "acme-org/my-project");
    }

    /// Why: a workspace with no git remote must return None gracefully.
    /// Test: itself.
    #[test]
    fn derive_source_id_none_for_no_remote() {
        let tmp = TempDir::new().expect("tempdir");
        let dir = tmp.path();
        Command::new("git")
            .arg("-C")
            .arg(dir)
            .arg("init")
            .output()
            .ok();
        let record = make_record_with_workspace(dir.to_path_buf());
        // No remote configured → derive must return None (never panic).
        assert!(derive_source_id_for_record(&record).is_none());
    }

    /// Why: externally-adopted sessions with cwd="/unknown" and no workspace_path
    /// must return None gracefully — never error the reconcile loop.
    /// Test: itself.
    #[test]
    fn derive_source_id_none_for_unknown_path() {
        let record = make_record_unknown_cwd();
        assert!(derive_source_id_for_record(&record).is_none());
    }
}
