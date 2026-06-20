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
//! [`SessionManager::adopt_existing`].
//! Test: `manager_adopt_existing_*` in `super::tests`.

use std::path::PathBuf;

use chrono::Utc;
use tracing::info;

use super::manager::{ManagedError, SessionManager};
use super::record::{ManagedSessionId, ManagedSessionState, SessionRecord};

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
    /// - **No `create_session` call.** The pane already exists; the driver is only
    ///   consulted via `session_exists` to verify presence.
    /// - **`cwd` is REQUIRED** (a plain `PathBuf`): the pane's provenance is unknown
    ///   to the daemon, so the operator supplies the working directory rather than
    ///   have it stubbed to `/unknown` (as auto-boot adoption does). `task` may be
    ///   empty. `runtime` is caller-supplied; call sites that do not care pass
    ///   [`crate::runtime::RuntimeKind::default`].
    /// - **Non-`tmpm-` names are ALLOWED.** Unlike `reconcile_on_boot` (which filters
    ///   to the `tmpm-` prefix for SAFE automatic adoption), this explicit path
    ///   adopts ANY name the operator names. The reconcile filter is left untouched.
    ///
    /// What: verifies the pane exists, rejects an already-tracked name, then upserts
    /// an `Active` [`SessionRecord`] carrying the supplied `cwd`/`task`/`runtime`
    /// (a fresh id, no workspace/repo/branch — provenance is unknown). Returns the
    /// new record.
    /// Test: `manager_adopt_existing_registers_active`,
    /// `manager_adopt_existing_missing_tmux_errors`,
    /// `manager_adopt_existing_double_adopt_errors`,
    /// `manager_adopt_existing_allows_non_tmpm_name` in `super::tests`.
    pub async fn adopt_existing(
        &self,
        tmux_name: &str,
        cwd: PathBuf,
        task: String,
        runtime: crate::runtime::RuntimeKind,
    ) -> Result<SessionRecord, ManagedError> {
        // The pane MUST already exist — adoption connects, it does not spawn.
        // `tmux_driver()` is the public accessor over the shared driver Arc.
        if !self.tmux_driver().session_exists(tmux_name) {
            return Err(ManagedError::TmuxSessionMissing(tmux_name.to_string()));
        }

        // Reject an already-tracked name so we never create a second record for the
        // same pane. `known_tmux_names` reloads-on-read so a record another process
        // registered is also seen.
        if self.known_tmux_names().await?.contains(tmux_name) {
            return Err(ManagedError::AlreadyAdopted(tmux_name.to_string()));
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
        };

        self.store.write().await.upsert(record.clone()).await?;
        info!(
            id = %record.id,
            name = %tmux_name,
            runtime = %runtime.as_str(),
            "adopted existing tmux session into the managed store"
        );
        Ok(record)
    }
}
