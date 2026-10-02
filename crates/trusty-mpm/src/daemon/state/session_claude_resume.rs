//! Rebinding a session id to the `claude` the daemon itself resumed (#8983).
//!
//! Why: `claude --resume <id>` keeps the session id but runs a new process.
//! The id is already bound to the old `claude`, which fails the pid +
//! start-time check, so a resumed session could not clear its own delegation
//! records until the 6 h stale path. A sibling can announce the same id from
//! a process it names `claude`, so the new process must be proven by
//! something a sibling cannot write.
//! What: before a resume path sends its launch line, it records a
//! [`ResumeGrant`] — the pane and the instant. A later socket `SessionStart`
//! for that id rebinds it only when the kernel-verified announcing `claude`
//! IS the `claude` running in that pane (pid AND start time) and started
//! after the grant. Any other announcer, or a lookup that cannot answer,
//! rebinds nothing.
//! Test: `session_claudes_tests.rs` (`_8983` cases).

use std::time::{SystemTime, UNIX_EPOCH};

use super::DaemonState;
use super::session_claudes::ResumeGrant;
use crate::core::session::SessionId;
use crate::core::twin_identity::ClaudeProcess;

impl DaemonState {
    /// Grant `record`'s resumed `claude` its session id (#8983).
    ///
    /// What: [`Self::grant_resumed_session`] over the record's stored
    /// `claude_session_id`, tmux session and pane.
    /// Test: `a_daemon_resumed_claude_rebinds_its_session_8983`.
    pub fn grant_resume_of(&self, record: &crate::session_manager::SessionRecord) {
        self.grant_resumed_session(
            record.claude_session_id.as_deref(),
            &record.tmux_name,
            record.pane_id.as_deref(),
        );
    }

    /// Record that the daemon is about to relaunch `claude_session_id` in
    /// `tmux_name`'s pane (#8983).
    ///
    /// What: a [`ResumeGrant`] stamped now. No grant for a missing or
    /// malformed id: such a launch starts a fresh session, whose own
    /// `SessionStart` binds it.
    /// Test: `a_malformed_resume_id_grants_nothing_8983`.
    pub fn grant_resumed_session(
        &self,
        claude_session_id: Option<&str>,
        tmux_name: &str,
        pane_id: Option<&str>,
    ) {
        let Some(session) = claude_session_id
            .and_then(|id| uuid::Uuid::parse_str(id).ok())
            .map(SessionId)
        else {
            return;
        };
        let issued_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        self.session_claudes.grant_resume(
            session,
            ResumeGrant {
                tmux_name: tmux_name.to_string(),
                pane_id: pane_id.map(str::to_string),
                issued_at,
            },
        );
    }

    /// Rebind `session` to `claude`, the kernel-verified sender of its
    /// `SessionStart`, when the daemon resumed it there (#8983).
    ///
    /// What: `Ok` and the grant consumed when `session` has a grant, `claude`
    /// started no earlier than it, and `pane_claude` names exactly `claude`
    /// as the `claude` in the granted pane. `Err` naming the failed step
    /// otherwise — no grant, an earlier start, a pane lookup that fails or
    /// names another process, a sealed registry, a failed save — and the
    /// binding is left as it was.
    /// Test: `a_daemon_resumed_claude_rebinds_its_session_8983`,
    /// `a_sibling_claude_announcing_a_resumed_id_is_not_bound_8983`,
    /// `a_resume_rebind_fails_closed_8983`.
    pub(crate) fn rebind_resumed_claude_with(
        &self,
        session: SessionId,
        claude: ClaudeProcess,
        pane_claude: impl Fn(&ResumeGrant) -> Result<ClaudeProcess, String>,
    ) -> Result<(), String> {
        let claudes = &self.session_claudes;
        let grant = claudes
            .resume_grant(session)
            .ok_or("the daemon has not resumed this session")?;
        if claude.start_time < grant.issued_at {
            return Err(format!(
                "claude pid {} started before the daemon resumed the session",
                claude.pid
            ));
        }
        let launched = pane_claude(&grant).map_err(|e| {
            format!(
                "the claude in the daemon's pane for {} could not be found: {e}",
                grant.tmux_name
            )
        })?;
        if launched != claude {
            return Err(format!(
                "claude pid {} is not the claude the daemon resumed in {} (pid {})",
                claude.pid, grant.tmux_name, launched.pid
            ));
        }
        claudes.rebind(session, claude)?;
        claudes.revoke_resume(session, &grant);
        Ok(())
    }
}

/// The `claude` running in `grant`'s pane, with its start time (#8983).
pub(crate) fn pane_claude(grant: &ResumeGrant) -> Result<ClaudeProcess, String> {
    let pid = match grant.pane_id.as_deref() {
        Some(pane) => crate::core::process::find_claude_pid_in_pane(&grant.tmux_name, pane),
        None => crate::core::process::find_claude_pid_in_tmux(
            &grant.tmux_name,
            1,
            std::time::Duration::ZERO,
        ),
    }
    .ok_or("no claude runs in the pane")?;
    let facts = crate::core::twin_arming::process_facts(pid)?;
    Ok(ClaudeProcess {
        pid,
        start_time: facts.start_time,
    })
}
