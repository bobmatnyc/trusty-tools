//! Derive the `session_id` a pause is attributed to when the caller supplies
//! none.
//!
//! Why: #6888 — `session_context_pause` took `session_id` as free text and the
//! pause skill told the PM to invent one ("`$TM_SESSION_ID`, or the tmux
//! `session:window`, or any other stable value"). Nothing in this workspace
//! exports `$TM_SESSION_ID`, so the PM picked a different string on nearly
//! every pause: one checkout's `sessions-log.jsonl` carries over forty distinct
//! ids in five incompatible shapes for what is one continuous stream of work.
//! The resume lookup is exact string equality
//! ([`resolve_session_snapshot`](crate::catchup::session_log::resolve_session_snapshot)),
//! so the second guess never matches the first and the owner is told no
//! snapshot resolved for its session.
//! What: [`derive_session_id`] computes the id from what the caller IS rather
//! than from what it chose to type — the managed session id when the caller
//! runs in a `tm`-managed pane, else the stable tmux window id that
//! [`resolve_snapshot_for_caller`](crate::catchup::resolve::resolve_snapshot_for_caller)
//! already matches on. Writer and reader run this same function over the same
//! caller identity, so the two agree by construction instead of by the PM
//! guessing the same string twice.
//!
//! This does NOT reopen the "latest overall" fallback #5272 removed. A derived
//! id keys exactly one caller — one managed session, or one tmux window — and a
//! caller that is neither derives nothing at all rather than inheriting an
//! arbitrary session's state.
//! Test: inline `#[cfg(test)]` module.

use crate::catchup::resolve::window_id_of;

/// Environment variable carrying the managed session id inside a `tm` pane.
///
/// Why: named here so the derivation and its callers cannot drift apart on the
/// spelling. `TM_MANAGED_SESSION_ID` is the ONLY identifier of this family the
/// runtime actually exports (`runtime::claude_code`); `$TM_SESSION_ID`, which
/// the pause skill used to name, is set nowhere (#6888).
pub const MANAGED_SESSION_ID_ENV: &str = "TM_MANAGED_SESSION_ID";

/// Prefix for a session id derived from a tmux window.
///
/// Why: a bare `@230` is not a legal snapshot directory name
/// ([`session_dir_name`](crate::catchup::session_log::session_dir_name) rejects
/// `@`), so a bare window id would push every derived pause flat to the store
/// root. The prefix also says out loud, in the log and on disk, that the id was
/// derived from a window rather than minted by `tm`.
pub const TMUX_WINDOW_ID_PREFIX: &str = "tmux-window-";

/// The session id a caller's pause should be attributed to.
///
/// Why: #6888 — the writer and the reader have to arrive at the same string
/// from the same caller, and a PM-authored free-text id delivers that only when
/// the PM happens to retype it identically after a restart. Deriving it from
/// the caller's own identity removes the guess: both sides call this.
/// What: (1) `managed_session_id` — the `TM_MANAGED_SESSION_ID` a `tm`-managed
/// pane exports — wins whenever it is non-empty, because it names the session
/// itself and survives a Claude Code relaunch inside the pane; (2) failing
/// that, the caller's tmux window, reduced to the stable `@N` id
/// [`window_id_of`] extracts and rendered as `tmux-window-N` so it is a legal
/// directory name; (3) `None` when the caller is neither, which is the correct
/// answer — an unidentified caller owns nothing (#5272) and must pass an
/// explicit `session_id` instead of being handed someone else's.
///
/// A window id whose body is not ASCII alphanumeric derives nothing rather than
/// being mangled into a safe name: two windows collapsing onto one id would
/// recreate exactly the crosstalk #5272 removed.
/// Test: `managed_id_wins_over_the_window`,
/// `derives_a_directory_safe_id_from_a_tmux_window`,
/// `an_unidentified_caller_derives_nothing`,
/// `derived_pause_resolves_for_the_same_managed_caller`,
/// `derived_pause_resolves_for_the_same_tmux_caller`.
pub fn derive_session_id(
    managed_session_id: Option<&str>,
    tmux_window: Option<&str>,
) -> Option<String> {
    if let Some(id) = managed_session_id.map(str::trim).filter(|s| !s.is_empty()) {
        return Some(id.to_string());
    }
    tmux_window.and_then(tmux_window_session_id)
}

/// Render `session_name:window_index:@230` as `tmux-window-230`.
///
/// Why: the `@N` window id is the one component that survives a rename and a
/// renumber, and it is already what [`window_id_of`] compares — reusing it is
/// what makes the derived id and the window fallback name the same caller.
/// What: `None` unless the field parses to a window id whose body (after `@`)
/// is non-empty and ASCII alphanumeric.
/// Test: `derives_a_directory_safe_id_from_a_tmux_window`,
/// `a_malformed_window_derives_nothing`.
fn tmux_window_session_id(tmux_window: &str) -> Option<String> {
    let body = window_id_of(tmux_window)?.trim_start_matches('@');
    if body.is_empty() || !body.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    Some(format!("{TMUX_WINDOW_ID_PREFIX}{body}"))
}

/// The managed session id of THIS process, when it runs inside a `tm` pane.
///
/// Why: only the process Claude Code itself spawned — the `trusty-mpm serve
/// --stdio` bridge — inherits the pane's exported `TM_MANAGED_SESSION_ID`. The
/// daemon behind that bridge is a different, long-lived process, so a daemon
/// that happened to be auto-started from inside some managed pane would
/// otherwise stamp every caller's pause with that one stale id. Reading the
/// variable is therefore the bridge's job, and this function exists so the
/// bridge does not spell the variable name itself.
/// What: the trimmed value of [`MANAGED_SESSION_ID_ENV`], or `None` when it is
/// unset or empty.
/// Test: `managed_session_id_from_env_reads_the_variable`.
pub fn managed_session_id_from_env() -> Option<String> {
    std::env::var(MANAGED_SESSION_ID_ENV)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catchup::pause::{PauseSnapshotInput, write_pause_snapshot};
    use crate::catchup::resolve::{ResolutionPath, resolve_snapshot_for_caller};
    use crate::catchup::session_log::session_dir_name;
    use tempfile::TempDir;

    fn pause(dir: &std::path::Path, session_id: &str, tmux_window: Option<&str>) {
        let input = PauseSnapshotInput {
            session_id,
            summary: "Paused.",
            completed: &[],
            in_progress: &[],
            next_steps: &[],
            tmux_window,
        };
        write_pause_snapshot(dir, &input).unwrap();
    }

    #[test]
    fn managed_id_wins_over_the_window() {
        assert_eq!(
            derive_session_id(Some("mgr-1"), Some("proj:0:@230")).as_deref(),
            Some("mgr-1")
        );
        // Blank is not an identity.
        assert_eq!(
            derive_session_id(Some("   "), Some("proj:0:@230")).as_deref(),
            Some("tmux-window-230")
        );
    }

    #[test]
    fn derives_a_directory_safe_id_from_a_tmux_window() {
        let id = derive_session_id(None, Some("tm-dogfood:0:@230")).unwrap();
        assert_eq!(id, "tmux-window-230");
        assert_eq!(
            session_dir_name(&id),
            Some(id.as_str()),
            "a derived id must be usable as a snapshot directory name"
        );
    }

    #[test]
    fn a_malformed_window_derives_nothing() {
        for bad in ["", "proj", "a:b:c:d", "proj:0:@", "proj:0:@2 3"] {
            assert_eq!(derive_session_id(None, Some(bad)), None, "{bad:?}");
        }
    }

    #[test]
    fn an_unidentified_caller_derives_nothing() {
        assert_eq!(derive_session_id(None, None), None);
        assert_eq!(derive_session_id(Some(""), None), None);
    }

    #[test]
    fn managed_session_id_from_env_reads_the_variable() {
        // Read-only assertion about the current environment: whatever the
        // variable holds, the helper agrees with it. No `set_var` — this test
        // shares a process with every other test in the binary.
        let expected = std::env::var(MANAGED_SESSION_ID_ENV)
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty());
        assert_eq!(managed_session_id_from_env(), expected);
    }

    /// Why: #6888 — the whole point is that the writer and the reader arrive at
    /// the same id from the same caller. A managed caller pauses without ever
    /// naming an id, and the next resume from that same caller must resolve the
    /// snapshot through the exact-id route, not through a fallback.
    /// What: derive → pause → derive again → resolve, with no tmux window
    /// recorded on the snapshot at all, so the pre-existing window fallback
    /// cannot be what answers.
    /// Test: itself.
    #[test]
    fn derived_pause_resolves_for_the_same_managed_caller() {
        let tmp = TempDir::new().unwrap();
        let managed = Some("7bd5c27a-475b-41df-9e9f-a6f630801717");

        let write_id = derive_session_id(managed, None).unwrap();
        pause(tmp.path(), &write_id, None);

        let read_id = derive_session_id(managed, None).unwrap();
        let resolved = resolve_snapshot_for_caller(tmp.path(), Some(&read_id), None)
            .expect("a managed caller must resolve the pause it just wrote");
        assert_eq!(resolved.via, ResolutionPath::SessionId);
        assert!(resolved.path.exists());
    }

    /// Why: #6888 — a non-managed caller's identity is its tmux window, and a
    /// relaunch inside that window mints a new harness id but keeps the window.
    /// Deriving from the window is what makes the exact-id route survive it.
    /// What: two callers in the SAME window resolve the same snapshot; a caller
    /// in a DIFFERENT window resolves nothing, so the derivation still keys one
    /// caller and never degrades to "latest overall" (#5272).
    /// Test: itself.
    #[test]
    fn derived_pause_resolves_for_the_same_tmux_caller() {
        let tmp = TempDir::new().unwrap();

        // The snapshot records NO window, so only the derived session id can
        // answer — the pre-existing window fallback has nothing to match.
        let write_id = derive_session_id(None, Some("tm-dogfood:0:@230")).unwrap();
        pause(tmp.path(), &write_id, None);

        // Same window, renamed session and renumbered index after a relaunch.
        let read_id = derive_session_id(None, Some("renamed:7:@230")).unwrap();
        let resolved = resolve_snapshot_for_caller(tmp.path(), Some(&read_id), None)
            .expect("the same tmux window must resolve its own pause");
        assert_eq!(resolved.via, ResolutionPath::SessionId);

        let other = derive_session_id(None, Some("tm-dogfood:0:@999")).unwrap();
        assert!(
            resolve_snapshot_for_caller(tmp.path(), Some(&other), None).is_none(),
            "another window must never inherit this snapshot (#5272)"
        );
    }

    /// Why: #6888 must not change what an explicit `session_id` means — every
    /// existing caller that passes one keeps resolving exactly as before.
    /// What: an explicitly named id round-trips through the same writer and
    /// reader, and a derived id does not collide with it.
    /// Test: itself.
    #[test]
    fn an_explicit_session_id_still_round_trips() {
        let tmp = TempDir::new().unwrap();
        pause(tmp.path(), "explicit-id", None);

        let resolved = resolve_snapshot_for_caller(tmp.path(), Some("explicit-id"), None)
            .expect("an explicit session id still resolves its own pause");
        assert_eq!(resolved.via, ResolutionPath::SessionId);

        let derived = derive_session_id(None, Some("tm-dogfood:0:@230")).unwrap();
        assert!(
            resolve_snapshot_for_caller(tmp.path(), Some(&derived), None).is_none(),
            "a derived id must not pick up an explicitly attributed snapshot"
        );
    }
}
