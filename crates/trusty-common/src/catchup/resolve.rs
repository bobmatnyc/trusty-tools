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
//! [`redact_sessions_not_owned_by`] applies the same ownership test to the
//! digest, so the response cannot hand out the material for a claim the caller
//! could not otherwise make (#5386).
//! Test: inline `#[cfg(test)]` module.

use std::path::{Path, PathBuf};

use crate::catchup::json::PausedSessionJson;
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
    // #5272: this is NOT the "latest overall" fallback that issue removed. That
    // one answered an unidentified caller with an arbitrary session's file; this
    // one requires the caller to be in the window that wrote the snapshot.
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
/// What: the last `:`-delimited component, accepted only when it looks like a
/// tmux window id — a leading `@` followed by at least one character. Taking
/// the LAST component rather than requiring exactly three is what lets a tmux
/// session name contain its own `:` (`my:proj:0:@7`); requiring the `@` is what
/// keeps that from degenerating into "whatever follows the final colon", so
/// `a:b:c:d` still parses to nothing. Empty, one-component and
/// empty-last-component inputs yield `None`, so they match nothing.
/// Test: `window_id_of_reads_the_third_component`,
/// `window_id_of_tolerates_colons_in_the_session_name`,
/// `malformed_window_fields_never_match`.
pub fn window_id_of(field: &str) -> Option<&str> {
    let id = field.trim().rsplit(':').next()?;
    (id.len() > 1 && id.starts_with('@')).then_some(id)
}

/// Who a catch-up caller claims to be.
///
/// Why: both ownership questions this module answers — "which snapshot do I
/// resume from" and "which listed sessions may I see in full" — take the same
/// two self-reported identifiers, and passing them as a pair keeps the two
/// answers from drifting apart.
/// What: `session_id` is the harness session id; `tmux_window` is the caller's
/// own `session_name:window_index:window_id`. Both are self-reported and
/// neither is verified server-side — which is precisely why
/// [`redact_sessions_not_owned_by`] exists: the tool must not also SUPPLY the
/// values needed to make a claim.
/// Test: `redaction_withholds_handles_and_restorable_state`.
#[derive(Debug, Clone, Copy, Default)]
#[non_exhaustive]
pub struct CallerIdentity<'a> {
    /// The caller's own session id, when it identified itself.
    pub session_id: Option<&'a str>,
    /// The caller's own tmux window field, when it is running inside tmux.
    pub tmux_window: Option<&'a str>,
}

impl<'a> CallerIdentity<'a> {
    /// Build an identity from the two MCP arguments.
    pub fn new(session_id: Option<&'a str>, tmux_window: Option<&'a str>) -> Self {
        Self {
            session_id,
            tmux_window,
        }
    }
}

/// Withhold, from every session the caller does not own, the fields that would
/// let it adopt that session.
///
/// Why: #5272 removed the "latest overall" fallback so an unidentified caller
/// could not inherit an arbitrary session's state. It left the DATA that
/// reconstructs the same result by hand: the digest returned every paused
/// session's `source_file` and `tmux_window` to any caller, so a caller could
/// read another session's window out of one response, hand it back as its own,
/// and resolve that session's snapshot deterministically — or skip the tool and
/// read `source_file` off disk. Removing one code path while still publishing
/// the means to re-create it is not the invariant #5272 was defending, so the
/// response now honors it directly.
///
/// What: ownership is the same claim [`resolve_snapshot_for_caller`] accepts —
/// the session is attributed to `caller.session_id` in `sessions-log.jsonl` (or
/// sits in that id's directory), OR the caller is in the tmux window that paused
/// it. For everything else the entry keeps `format`, `paused_at` and `summary`
/// and loses the rest, with `owned: false` saying so. The line is drawn at what
/// a resuming PM would ACT on: `source_file`/`tmux_window` are the handles that
/// load a snapshot, and `in_progress`/`next_steps`/`git_context` are the state
/// `/tm-session-resume` restores as its own todos. `summary` stays because
/// "something else paused here, and it was about X" is the digest's purpose and
/// nothing loads from it.
///
/// A legacy claude-mpm session carries no id attribution and no window, so it is
/// never owned. It also carries no `source_file`, so nothing actionable is
/// withheld — only its `in_progress`/`next_steps` fold.
/// Test: `redaction_withholds_handles_and_restorable_state`,
/// `owner_sees_every_field`, `window_owner_sees_every_field`,
/// `redaction_leaves_nothing_to_reconstruct_a_window_claim_from`.
pub fn redact_sessions_not_owned_by(
    project_dir: &Path,
    caller: &CallerIdentity<'_>,
    sessions: &mut [PausedSessionJson],
) {
    let owned_paths = caller
        .session_id
        .map(|id| {
            let sessions_dir = project_dir.join(".trusty-mpm").join("sessions");
            crate::catchup::session_log::snapshots_attributed_to(&sessions_dir, id, "md")
                .iter()
                .map(|p| canonical(p))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let caller_window = caller.tmux_window.and_then(window_id_of);

    for s in sessions.iter_mut() {
        if !is_owned_by(s, &owned_paths, caller_window) {
            withhold(s);
        }
    }
}

/// Whether one digest entry is attributable to the caller.
///
/// Why: the two ownership routes have to agree with
/// [`resolve_snapshot_for_caller`], or a caller could resolve a snapshot whose
/// own digest entry it is not allowed to read.
/// What: true when the entry's `source_file` is one of the caller's attributed
/// snapshots, or its recorded window id equals the caller's.
/// Test: `owner_sees_every_field`, `window_owner_sees_every_field`.
fn is_owned_by(
    session: &PausedSessionJson,
    owned_paths: &[PathBuf],
    caller_window: Option<&str>,
) -> bool {
    if let Some(file) = session.source_file.as_deref() {
        let path = canonical(Path::new(file));
        if owned_paths.contains(&path) {
            return true;
        }
    }
    match (
        session.tmux_window.as_deref().and_then(window_id_of),
        caller_window,
    ) {
        (Some(recorded), Some(mine)) => recorded == mine,
        _ => false,
    }
}

/// Strip a digest entry down to what a non-owning caller may see.
///
/// Why: kept separate from the predicate so the disclosure boundary is one
/// readable list rather than five scattered assignments.
/// What: clears the two handles and the three restorable-state fields, and
/// marks the entry unowned. `format`, `paused_at` and `summary` survive.
/// Test: `redaction_withholds_handles_and_restorable_state`.
fn withhold(session: &mut PausedSessionJson) {
    session.source_file = None;
    session.tmux_window = None;
    session.in_progress = None;
    session.next_steps = None;
    session.git_context = None;
    session.owned = false;
}

/// Resolve a path for comparison, falling back to the path itself.
///
/// Why: the digest builds snapshot paths by joining `project_dir`, while the
/// attribution index joins the store root; a symlinked or non-normalised
/// `project_dir` would make two spellings of one file compare unequal and
/// redact a caller's own session.
/// What: [`std::fs::canonicalize`], or the input unchanged when it cannot be
/// resolved (a path that no longer exists compares by spelling, as before).
/// Test: covered by `owner_sees_every_field`.
fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
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

    /// Why: tmux permits a literal `:` in a session name, which pushes the
    /// field past three components. Requiring exactly three made a caller's own
    /// snapshot unmatchable — it failed closed, so it was a functionality gap
    /// rather than a leak, but it is the caller's OWN window that stops working.
    /// What: the window id is read from the last component regardless of how
    /// many precede it, while a last component that is not an `@id` still
    /// parses to nothing.
    /// Test: itself.
    #[test]
    fn window_id_of_tolerates_colons_in_the_session_name() {
        assert_eq!(window_id_of("my:proj:0:@7"), Some("@7"));
        assert_eq!(window_id_of("a:b:c:d:e:12:@230"), Some("@230"));
        assert_eq!(
            window_id_of("a:b:c:d"),
            None,
            "a non-@ tail is not a window"
        );

        let tmp = tempfile::TempDir::new().unwrap();
        pause(tmp.path(), "writer", Some("my:proj:0:@7"));
        let got = resolve_snapshot_for_caller(tmp.path(), None, Some("my:proj:0:@7"))
            .expect("a colon in the session name must not break the caller's own match");
        assert_eq!(got.via, ResolutionPath::TmuxWindow);
    }

    /// Build the digest entry `generate_catchup_json` would produce for a real
    /// snapshot file.
    fn entry(path: &Path, window: Option<&str>) -> PausedSessionJson {
        PausedSessionJson {
            format: "trusty-mpm".to_string(),
            paused_at: None,
            summary: "work".to_string(),
            in_progress: Some("halfway through X".to_string()),
            next_steps: Some("finish X".to_string()),
            git_context: Some("branch: main".to_string()),
            tmux_window: window.map(str::to_string),
            source_file: Some(path.display().to_string()),
            owned: true,
        }
    }

    /// Why: #5386 — the digest returned every paused session's `source_file`
    /// and `tmux_window` to any caller, so a caller could read another
    /// session's window out of the response, hand it back as its own, and
    /// resolve that session's snapshot deterministically. #5272 removed the
    /// code path that did this automatically; leaving the material to redo it
    /// by hand is not the invariant it was defending.
    /// What: a caller owning nothing sees the session exists and what it was
    /// about, and loses both handles plus the state a resume would restore.
    /// Test: itself.
    #[test]
    fn redaction_withholds_handles_and_restorable_state() {
        let tmp = tempfile::TempDir::new().unwrap();
        let theirs = pause(tmp.path(), "theirs", Some("tm-dogfood:0:@230"));
        let mut sessions = vec![entry(&theirs, Some("tm-dogfood:0:@230"))];

        redact_sessions_not_owned_by(
            tmp.path(),
            &CallerIdentity::new(Some("nobody"), Some("other:1:@999")),
            &mut sessions,
        );

        let s = &sessions[0];
        assert!(!s.owned, "a session the caller does not own must say so");
        assert_eq!(s.source_file, None, "the snapshot path is a handle");
        assert_eq!(s.tmux_window, None, "the window is a handle");
        assert_eq!(s.in_progress, None);
        assert_eq!(s.next_steps, None);
        assert_eq!(s.git_context, None);
        // Still enough to answer "something else paused here".
        assert_eq!(s.summary, "work");
        assert_eq!(s.format, "trusty-mpm");
    }

    /// Why: the exploit the redaction closes is specifically "read the window
    /// out of this response, then send it back" — so the response must carry no
    /// spelling of another session's window at all.
    /// What: nothing in the redacted entry, serialized, contains the window id
    /// that would resolve the snapshot.
    /// Test: itself.
    #[test]
    fn redaction_leaves_nothing_to_reconstruct_a_window_claim_from() {
        let tmp = tempfile::TempDir::new().unwrap();
        let theirs = pause(tmp.path(), "theirs", Some("tm-dogfood:0:@230"));
        let mut sessions = vec![entry(&theirs, Some("tm-dogfood:0:@230"))];

        redact_sessions_not_owned_by(tmp.path(), &CallerIdentity::default(), &mut sessions);

        let wire = serde_json::to_string(&sessions[0]).unwrap();
        assert!(
            !wire.contains("@230"),
            "the window id must not survive anywhere on the wire: {wire}"
        );
        assert!(
            !wire.contains(theirs.to_str().unwrap()),
            "the snapshot path must not survive anywhere on the wire: {wire}"
        );
    }

    /// Why: redaction that also hid the caller's OWN session would break resume
    /// outright — the digest is what `/tm-session-resume` renders.
    /// What: the session id that paused the snapshot keeps every field.
    /// Test: itself.
    #[test]
    fn owner_sees_every_field() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mine = pause(tmp.path(), "mine", Some("tm-dogfood:0:@230"));
        let mut sessions = vec![entry(&mine, Some("tm-dogfood:0:@230"))];

        redact_sessions_not_owned_by(
            tmp.path(),
            &CallerIdentity::new(Some("mine"), None),
            &mut sessions,
        );

        let s = &sessions[0];
        assert!(s.owned);
        assert_eq!(s.source_file.as_deref(), Some(mine.to_str().unwrap()));
        assert_eq!(s.tmux_window.as_deref(), Some("tm-dogfood:0:@230"));
        assert_eq!(s.next_steps.as_deref(), Some("finish X"));
    }

    /// Why: the window fallback and the digest must agree. A caller that can
    /// RESOLVE a snapshot by window but cannot READ its digest entry would see
    /// `resolved_snapshot` pointing at a row it was told it does not own.
    /// What: the window that paused the snapshot keeps every field, with no
    /// session id at all.
    /// Test: itself.
    #[test]
    fn window_owner_sees_every_field() {
        let tmp = tempfile::TempDir::new().unwrap();
        let theirs = pause(
            tmp.path(),
            "some-earlier-incarnation",
            Some("tm-dogfood:0:@230"),
        );
        let mut sessions = vec![entry(&theirs, Some("tm-dogfood:0:@230"))];

        redact_sessions_not_owned_by(
            tmp.path(),
            &CallerIdentity::new(Some("relaunched-id"), Some("renamed:7:@230")),
            &mut sessions,
        );

        assert!(sessions[0].owned, "the window that paused it owns it");
        assert!(sessions[0].source_file.is_some());
    }

    /// Why: a snapshot with no `source_file` and no window — every legacy
    /// claude-mpm entry — is attributable to nobody, so it must not be owned by
    /// default. Defaulting it to owned would exempt the whole legacy arm.
    /// What: an entry with neither handle is redacted for every caller.
    /// Test: itself.
    #[test]
    fn an_unattributable_session_is_owned_by_nobody() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut sessions = vec![PausedSessionJson {
            format: "claude-mpm".to_string(),
            paused_at: None,
            summary: "legacy work".to_string(),
            in_progress: Some("todo 1".to_string()),
            next_steps: None,
            git_context: None,
            tmux_window: None,
            source_file: None,
            owned: true,
        }];

        redact_sessions_not_owned_by(
            tmp.path(),
            &CallerIdentity::new(Some("anyone"), Some("tm-dogfood:0:@230")),
            &mut sessions,
        );

        assert!(!sessions[0].owned);
        assert_eq!(sessions[0].in_progress, None);
        assert_eq!(sessions[0].summary, "legacy work");
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
