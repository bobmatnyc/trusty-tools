//! Which paused snapshot a resuming caller should reload, and by which route.
//!
//! Why: relaunching Claude Code inside the same tmux window mints a NEW harness
//! session id, so an exact-id lookup misses every snapshot the previous
//! incarnation wrote and `/tm-session-resume` degrades to a human reading prose
//! summaries to guess which of N snapshots is theirs. The tmux WINDOW ID is the
//! one identifier that survives the relaunch, and the pause path already records
//! it.
//! What: [`resolve_snapshot_for_caller`] tries the exact `session_id` first and,
//! only on a miss, the newest paused snapshot in the same project whose recorded
//! `tmux_window` carries the caller's window id. [`ResolutionPath`] names which
//! of the two answered so a caller never reads a fallback as an exact match.
//! Test: inline `#[cfg(test)]` module.

use std::path::{Path, PathBuf};

use crate::catchup::session_finder::{PausedSession, find_paused_sessions};

/// How a snapshot was arrived at.
///
/// Why: a fallback and an exact match are different claims about ownership, and
/// a caller that cannot tell them apart will present a guess as a certainty.
/// What: [`ResolutionPath::as_str`] gives the wire value used in the
/// `session_context_catchup` response's `resolved_via` field.
/// Test: `window_fallback_reports_its_resolution_path`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ResolutionPath {
    /// The caller's `session_id` owns this snapshot outright.
    SessionId,
    /// The caller runs in the tmux window that wrote this snapshot.
    TmuxWindow,
}

impl ResolutionPath {
    /// The wire value for the `resolved_via` response field.
    pub fn as_str(self) -> &'static str {
        match self {
            ResolutionPath::SessionId => "session_id",
            ResolutionPath::TmuxWindow => "tmux_window",
        }
    }
}

/// A snapshot plus the route that found it.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ResolvedSnapshot {
    /// Absolute path to the snapshot file.
    pub path: PathBuf,
    /// Which lookup answered.
    pub via: ResolutionPath,
}

impl ResolvedSnapshot {
    /// Pair a snapshot path with the route that found it.
    pub fn new(path: PathBuf, via: ResolutionPath) -> Self {
        Self { path, via }
    }
}

/// Resolve the snapshot a caller should resume from.
///
/// Why: #5272 removed the "latest overall" fallback because an unidentified
/// caller in a shared store would silently inherit an arbitrary session's state.
/// A tmux window id is not that guess — the caller is demonstrably running in
/// the window that wrote the snapshot, which is an ownership claim, not a
/// coin flip. Without it a relaunch in the same window resolves nothing: the
/// harness mints a new session id per launch, so every relaunch orphans another
/// session directory and the next resume matches nothing again.
///
/// Window ids CAN be reused: kill a window and tmux may hand `@230` to a new
/// one, which would then match the dead window's snapshot. That is an accepted,
/// bounded risk — the project-path scope below is the second gate, since the
/// scan only ever reads `project_dir`'s own store. There is deliberately no
/// liveness check on the recorded window.
/// What: (1) the exact `session_id` match via
/// [`latest_trusty_mpm_snapshot`](crate::catchup::session_finder::latest_trusty_mpm_snapshot),
/// which always wins; (2) failing that, the newest paused snapshot under
/// `project_dir` whose `## Tmux Window` section carries the caller's window id;
/// (3) otherwise `None`. A snapshot with no recorded window, or with a window
/// field that does not parse, never matches.
/// Test: `exact_session_id_match_wins_over_window_match`,
/// `window_fallback_resolves_when_session_id_never_paused`,
/// `window_fallback_is_scoped_to_the_project_dir`,
/// `malformed_window_fields_never_match`.
pub fn resolve_snapshot_for_caller(
    project_dir: &Path,
    session_id: Option<&str>,
    tmux_window: Option<&str>,
) -> Option<ResolvedSnapshot> {
    if let Some(path) =
        crate::catchup::session_finder::latest_trusty_mpm_snapshot(project_dir, session_id)
    {
        return Some(ResolvedSnapshot::new(path, ResolutionPath::SessionId));
    }
    let caller_window = window_id_of(tmux_window?)?;
    newest_snapshot_in_window(project_dir, caller_window)
        .map(|path| ResolvedSnapshot::new(path, ResolutionPath::TmuxWindow))
}

/// The newest paused snapshot under `project_dir` recorded against `window_id`.
///
/// Why: [`find_paused_sessions`] already sorts newest-first and already scopes
/// itself to one project's store, so "newest in this window, in this project" is
/// the first match over that list.
/// What: skips the legacy claude-mpm arm (it records no window) and every
/// snapshot whose window field is absent or unparseable.
/// Test: `window_fallback_resolves_when_session_id_never_paused`,
/// `snapshot_without_a_recorded_window_is_skipped`.
fn newest_snapshot_in_window(project_dir: &Path, window_id: &str) -> Option<PathBuf> {
    find_paused_sessions(project_dir)
        .ok()?
        .into_iter()
        .find_map(|s| match s {
            PausedSession::TrustyMpm {
                path,
                tmux_window: Some(w),
                ..
            } if window_id_of(&w) == Some(window_id) => Some(path),
            _ => None,
        })
}

/// Extract the stable window id from a `session_name:window_index:window_id`
/// field.
///
/// Why: matching on `session_name:window_index` would be wrong — session names
/// get renamed and window indexes renumber when a window is closed. The `@N`
/// window id is stable for the window's lifetime, so it is the only component
/// worth comparing.
/// What: the third component of a three-component field, or the whole string
/// when it is a bare `@N`. Empty, one-component, two-component, four-component
/// and empty-third-component inputs yield `None`, so they match nothing.
/// Test: `window_id_of_reads_the_third_component`,
/// `malformed_window_fields_never_match`.
pub fn window_id_of(field: &str) -> Option<&str> {
    let parts: Vec<&str> = field.trim().split(':').collect();
    match parts[..] {
        [_, _, id] if !id.is_empty() => Some(id),
        [id] if id.len() > 1 && id.starts_with('@') => Some(id),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catchup::pause::{PauseSnapshotInput, write_pause_snapshot};

    fn pause(dir: &Path, session_id: &str, window: Option<&str>) -> PathBuf {
        let input = PauseSnapshotInput {
            session_id,
            summary: "work",
            completed: &[],
            in_progress: &[],
            next_steps: &[],
            tmux_window: window,
        };
        write_pause_snapshot(dir, &input).unwrap().snapshot_path
    }

    #[test]
    fn window_id_of_reads_the_third_component() {
        assert_eq!(window_id_of("tm-dogfood:0:@230"), Some("@230"));
        assert_eq!(window_id_of("  main:12:@7  "), Some("@7"));
        assert_eq!(window_id_of("@230"), Some("@230"));
    }

    /// Why: the field is parsed from a file any process can write, and older
    /// snapshots carry shapes this code never produced. A panic or a loose
    /// match here would either crash resume or hand over someone else's state.
    /// What: every degenerate shape resolves to `None`, and a caller holding
    /// one of them resolves no snapshot at all.
    /// Test: itself.
    #[test]
    fn malformed_window_fields_never_match() {
        for bad in ["", "   ", "main", "main:0", "a:b:", "a:b:c:d", ":", "::"] {
            assert_eq!(window_id_of(bad), None, "{bad:?} must not parse");
        }

        let tmp = tempfile::TempDir::new().unwrap();
        pause(tmp.path(), "writer", Some("tm-dogfood:0:@230"));
        for bad in ["", "main", "main:0", "a:b:c:d"] {
            assert!(
                resolve_snapshot_for_caller(tmp.path(), Some("nobody"), Some(bad)).is_none(),
                "caller window {bad:?} must resolve nothing"
            );
        }
    }

    /// Why: the reported defect — a relaunch in the same tmux window mints a
    /// new harness session id, so the exact-id lookup misses and resume
    /// resolves nothing.
    /// What: an id that never paused plus the window that did resolves the
    /// window's newest snapshot, and says it came from the window.
    /// Test: itself.
    #[test]
    fn window_fallback_resolves_when_session_id_never_paused() {
        let tmp = tempfile::TempDir::new().unwrap();
        pause(tmp.path(), "old-incarnation", Some("tm-dogfood:0:@230"));

        let got = resolve_snapshot_for_caller(
            tmp.path(),
            Some("69895d04-149d-4c31-a640-29048831f9a5"),
            Some("tm-dogfood:0:@230"),
        )
        .expect("the window that paused must resolve its own snapshot");
        assert_eq!(got.via, ResolutionPath::TmuxWindow);
        assert!(got.path.is_file());
    }

    /// Why: matching on `session:index` would break the moment a window is
    /// renamed or renumbered; matching on `@id` must survive both.
    /// What: a caller whose session name and window index both differ still
    /// resolves, because the window id is the same.
    /// Test: itself.
    #[test]
    fn window_match_ignores_session_name_and_index() {
        let tmp = tempfile::TempDir::new().unwrap();
        pause(tmp.path(), "writer", Some("tm-dogfood:0:@230"));
        let got = resolve_snapshot_for_caller(tmp.path(), None, Some("renamed:7:@230"))
            .expect("the window id is what identifies the window");
        assert_eq!(got.via, ResolutionPath::TmuxWindow);
    }

    #[test]
    fn window_fallback_reports_its_resolution_path() {
        assert_eq!(ResolutionPath::SessionId.as_str(), "session_id");
        assert_eq!(ResolutionPath::TmuxWindow.as_str(), "tmux_window");
    }

    /// Why: #5272's rule is unchanged — an id that owns a snapshot gets that
    /// snapshot. The fallback is only reachable on a miss, so a window match
    /// can never displace an exact one.
    /// What: with both available, the exact id's own file wins and the route
    /// says `session_id`.
    /// Test: itself.
    #[test]
    fn exact_session_id_match_wins_over_window_match() {
        let tmp = tempfile::TempDir::new().unwrap();
        // A second session paused in the same window, under its own id.
        let mine = pause(tmp.path(), "mine", Some("tm-dogfood:0:@230"));
        let theirs = pause(tmp.path(), "theirs", Some("tm-dogfood:0:@230"));
        assert_ne!(mine, theirs);

        let got = resolve_snapshot_for_caller(tmp.path(), Some("mine"), Some("tm-dogfood:0:@230"))
            .expect("an owning id always resolves");
        assert_eq!(got.path, mine, "the exact id must not be overridden");
        assert_eq!(got.via, ResolutionPath::SessionId);
    }

    /// Why: window ids are reusable, so the project scope is the second gate.
    /// A snapshot in another project's store must stay invisible even when the
    /// window id matches exactly.
    /// What: the same window id resolves in the project that wrote it and
    /// nowhere else.
    /// Test: itself.
    #[test]
    fn window_fallback_is_scoped_to_the_project_dir() {
        let a = tempfile::TempDir::new().unwrap();
        let b = tempfile::TempDir::new().unwrap();
        pause(a.path(), "writer", Some("tm-dogfood:0:@230"));

        assert!(resolve_snapshot_for_caller(a.path(), None, Some("tm-dogfood:0:@230")).is_some());
        assert!(
            resolve_snapshot_for_caller(b.path(), None, Some("tm-dogfood:0:@230")).is_none(),
            "another project's store must not answer"
        );
    }

    /// Why: snapshots written before the window was captured have no
    /// `## Tmux Window` section at all; they must be skipped rather than
    /// matched or panicked on.
    /// What: a windowless snapshot is invisible to the fallback, and a newer
    /// windowed one still resolves past it.
    /// Test: itself.
    #[test]
    fn snapshot_without_a_recorded_window_is_skipped() {
        let tmp = tempfile::TempDir::new().unwrap();
        pause(tmp.path(), "legacy", None);
        assert!(resolve_snapshot_for_caller(tmp.path(), None, Some("tm-dogfood:0:@230")).is_none());

        let windowed = pause(tmp.path(), "modern", Some("tm-dogfood:0:@230"));
        let got = resolve_snapshot_for_caller(tmp.path(), None, Some("tm-dogfood:0:@230")).unwrap();
        assert_eq!(got.path, windowed);
    }

    #[test]
    fn no_session_id_and_no_window_resolves_nothing() {
        let tmp = tempfile::TempDir::new().unwrap();
        pause(tmp.path(), "writer", Some("tm-dogfood:0:@230"));
        assert!(resolve_snapshot_for_caller(tmp.path(), None, None).is_none());
        assert!(resolve_snapshot_for_caller(tmp.path(), Some("nobody"), None).is_none());
    }
}
