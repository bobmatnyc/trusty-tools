//! Unit tests for `session_finder`'s markdown section-extraction and
//! paused-session discovery/rendering helpers.
//!
//! Why: isolated in a sibling file (declared via `#[path =
//! "session_finder_tests.rs"] mod tests;` in `session_finder.rs`) to keep
//! `session_finder.rs` under the 500-SLOC production cap while retaining
//! full test coverage — the issue #3901 regression-test additions (a
//! corpus-representative header-style test plus the doc-comment rationale
//! for `extract_section`'s fail-open behavior) pushed the file over the cap.
//! As a child module, `super::` reaches private items in `session_finder`.
//!
//! What: exercises `extract_section`'s header-line-skip behavior (including
//! the issue #3901 regression fixtures and a representative sample of this
//! project's own `.trusty-mpm/sessions/*.md` annotated-header styles),
//! `parse_filename_timestamp`, paused-session discovery/ordering across the
//! trusty-mpm and claude-mpm formats, tmux-window round-tripping, and
//! `sessions-log.jsonl`-based latest-snapshot resolution.
//!
//! Test: `cargo test -p trusty-common --features catchup -- catchup::session_finder::tests`

use super::*;
use crate::catchup::mpm_session::ClaudeMpmSession;
use std::fs;
use tempfile::TempDir;

fn write_file(dir: &Path, name: &str, content: &str) -> PathBuf {
    let p = dir.join(name);
    fs::write(&p, content).unwrap();
    p
}

#[test]
fn parse_filename_timestamp_roundtrip() {
    let ts = parse_filename_timestamp("20260627-142030");
    assert!(ts.is_some());
    let ts = ts.unwrap();
    assert_eq!(ts.format("%Y-%m-%d").to_string(), "2026-06-27");
}

#[test]
fn parse_filename_timestamp_rejects_short() {
    assert!(parse_filename_timestamp("2026062").is_none());
    assert!(parse_filename_timestamp("").is_none());
}

/// Regression test for issue #5294: a stem with a multi-byte UTF-8 character
/// can satisfy the `len() == 15` / `len() == 8` byte-length guards while
/// still landing a fixed-byte-offset slice mid-character. Before the fix,
/// `stem="123é456-142030"` panicked with "byte index 4 is not a char
/// boundary" (verbatim reproduction from the issue, confirmed during
/// code-critic review of PR #5283). The fix must reject non-ASCII-digit
/// content before slicing rather than panic.
#[test]
fn parse_filename_timestamp_rejects_non_ascii_without_panicking() {
    assert!(parse_filename_timestamp("123é456-142030").is_none());
}

#[test]
fn extract_section_finds_content() {
    let md = "# Title\n\n## Summary\nDid lots of work.\n\n## Next Steps\nFix tests.";
    assert_eq!(
        extract_section(md, "Summary").as_deref(),
        Some("Did lots of work.")
    );
    assert_eq!(
        extract_section(md, "Next Steps").as_deref(),
        Some("Fix tests.")
    );
    assert!(extract_section(md, "Missing").is_none());
}

/// Regression test for issue #3901: a hand-written snapshot header with
/// trailing text (`## Next Steps (all Bob's call — none required)`) must
/// NOT leak that trailing text into the parsed body — the original
/// substring-`find` implementation absorbed `(all Bob's call — none
/// required)` as a prefix of the body. Fixture text is the actual
/// corrupted header shape from
/// `.trusty-mpm/sessions/session-20260721-020826.md` in this project.
/// The section is still extracted (not dropped) — see `extract_section`'s
/// doc comment for why a fail-closed `None` here is the wrong fix.
#[test]
fn extract_section_strips_trailing_header_annotation_from_body() {
    let md = "## In Progress\n\nNOTHING.\n\n\
               ## Next Steps (all Bob's call — none required)\n\n\
               - Bob's tested one-liner (handed over, not yet run).\n\
               - Installer NOT VERIFIED in isolation.\n";
    let next_steps = extract_section(md, "Next Steps").expect("section present");
    assert!(
        !next_steps.contains("all Bob's call"),
        "trailing header annotation leaked into body: {next_steps:?}"
    );
    assert!(next_steps.starts_with("- Bob's tested one-liner"));
    assert_eq!(
        extract_section(md, "In Progress").as_deref(),
        Some("NOTHING.")
    );
}

/// When the same header text appears twice, the first occurrence wins —
/// its own line is skipped and its body captured up through the second
/// occurrence's `## ` line, which acts as the body's end boundary. This
/// is deliberately simpler than an earlier draft that looped past
/// "malformed" (trailing-text) headers hunting for a later "well-formed"
/// one; that loop is exactly what caused the fail-closed regression
/// documented on `extract_section`. A duplicate same-named header does
/// not occur anywhere in this project's own 50-file
/// `.trusty-mpm/sessions/*.md` archive, so first-occurrence-wins is
/// preferred over defensive complexity for a scenario that has never
/// actually been observed.
#[test]
fn extract_section_first_occurrence_wins_when_header_repeated() {
    let md = "## Next Steps (draft, ignore)\nstale draft text\n\n\
               ## Next Steps\nReal next steps.\n";
    assert_eq!(
        extract_section(md, "Next Steps").as_deref(),
        Some("stale draft text")
    );
}

/// Trailing whitespace after the header (no visible extra text) is still
/// a well-formed header and must parse normally.
#[test]
fn extract_section_allows_trailing_whitespace_on_header_line() {
    let md = "## Next Steps   \nDo the thing.\n";
    assert_eq!(
        extract_section(md, "Next Steps").as_deref(),
        Some("Do the thing.")
    );
}

/// Representative sample of real annotated-header styles pulled from
/// this project's own `.trusty-mpm/sessions/*.md` archive (07-06 through
/// 07-24) — the corpus the fail-closed regression was found against.
/// Covers: a parenthetical annotation (the majority style,
/// `session-20260710-211500.md`), an em-dash annotation with no
/// parentheses (`session-20260720-104500.md`), and a clean unannotated
/// header (`session-20260714-064556.md`). All three must parse to
/// present, non-`None` content with no header-trailer text leaked in.
#[test]
fn extract_section_handles_representative_corpus_header_styles() {
    // Parenthetical annotation, from session-20260710-211500.md.
    let parenthetical = "## Completed\nDid stuff.\n\n\
        ## In Progress (BACKGROUND AGENTS — check GitHub for their PRs on resume)\n\
        #2108 BUILD WAVE 1 — three engineers dispatched ~20:45 EDT, isolated worktrees:\n\
        - #2117 status endpoint — DONE, PR #2396 OPEN awaiting review gate\n\n\
        ## Git Context\nBranch: main\n";
    let in_progress = extract_section(parenthetical, "In Progress").expect("present");
    assert!(!in_progress.contains("BACKGROUND AGENTS"));
    assert!(in_progress.starts_with("#2108 BUILD WAVE 1"));

    // Em-dash annotation with no parentheses, from
    // session-20260720-104500.md.
    let em_dash = "## Summary\nRelease chain.\n\n\
        ## In Progress — LIVE AGENTS AT PAUSE (verify via gh, not memory)\n\n\
        1. **Release chain — agent `a84bd2cbfb656d183`** (worktree\n\
        `.claude/worktrees/agent-a84bd2cbfb656d183`).\n\n\
        ## Git Context\nBranch: main\n";
    let in_progress = extract_section(em_dash, "In Progress").expect("present");
    assert!(!in_progress.contains("LIVE AGENTS AT PAUSE"));
    assert!(in_progress.starts_with("1. **Release chain"));

    // Clean, unannotated header, from session-20260714-064556.md.
    let clean = "## In Progress\nNOTHING in flight.\n\n\
        ## Next Steps (pending Bob decisions / next dispatches)\n\
        1. Cut tm 0.19.10? — phase 1 merged AFTER 0.19.9.\n";
    assert_eq!(
        extract_section(clean, "In Progress").as_deref(),
        Some("NOTHING in flight.")
    );
    let next_steps = extract_section(clean, "Next Steps").expect("present");
    assert!(!next_steps.contains("pending Bob decisions"));
    assert!(next_steps.starts_with("1. Cut tm 0.19.10?"));
}

#[test]
fn find_merges_both_formats() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path();

    // Create trusty-mpm session file.
    let tm_dir = project.join(".trusty-mpm").join("sessions");
    fs::create_dir_all(&tm_dir).unwrap();
    write_file(
        &tm_dir,
        "session-20260627-100000.md",
        "## Summary\nDone something.\n## Git Context\nbranch: main",
    );

    // Create claude-mpm session file.
    let cm_dir = project.join(".claude-mpm").join("sessions");
    fs::create_dir_all(&cm_dir).unwrap();
    write_file(
        &cm_dir,
        "session-20260626-090000.json",
        r#"{"session_id":"cm1","paused_at":"2026-06-26T09:00:00Z"}"#,
    );

    let sessions = find_paused_sessions(project).unwrap();
    assert_eq!(sessions.len(), 2);
    assert!(
        matches!(sessions[0], PausedSession::TrustyMpm { .. }),
        "newer trusty-mpm session should be first"
    );
    assert!(
        matches!(sessions[1], PausedSession::ClaudeMpm { .. }),
        "older claude-mpm session should be second"
    );
}

#[test]
fn find_orders_newest_first() {
    let tmp = TempDir::new().unwrap();
    let cm_dir = tmp.path().join(".claude-mpm").join("sessions");
    fs::create_dir_all(&cm_dir).unwrap();
    write_file(
        &cm_dir,
        "session-20260625-080000.json",
        r#"{"session_id":"old","paused_at":"2026-06-25T08:00:00Z"}"#,
    );
    write_file(
        &cm_dir,
        "session-20260627-100000.json",
        r#"{"session_id":"new","paused_at":"2026-06-27T10:00:00Z"}"#,
    );

    let sessions = find_paused_sessions(tmp.path()).unwrap();
    assert_eq!(sessions.len(), 2);
    let first_key = sessions[0].sort_key().unwrap();
    let second_key = sessions[1].sort_key().unwrap();
    assert!(first_key > second_key, "newest should be first");
}

#[test]
fn render_contains_digest_not_conversation() {
    let session = ClaudeMpmSession {
        session_id: "test-123".to_string(),
        paused_at: Some("2026-06-27T10:00:00Z".to_string()),
        resume_instructions: Some("Resume from step 3".to_string()),
        important_reminders: Some(vec!["Don't break prod".to_string()]),
        git_context: Some("branch: main".to_string()),
        ..Default::default()
    };
    let sessions = vec![PausedSession::ClaudeMpm { session }];
    let output = render_resume_context(&sessions);
    assert!(
        output.contains("Resume from step 3"),
        "resume instructions should be present"
    );
    assert!(
        output.contains("Don't break prod"),
        "reminders should be present"
    );
    assert!(
        output.contains("branch: main"),
        "git context should be present"
    );
    assert!(
        !output.contains("conversation"),
        "conversation must NOT appear in rendered output"
    );
}

#[test]
fn parse_extracts_tmux_window() {
    let tmp = TempDir::new().unwrap();
    let p = write_file(
        tmp.path(),
        "session-20260627-100000.md",
        "## Summary\nWork.\n\n## Tmux Window\nmain:2:@7\n\n## Git Context\nbranch: main",
    );
    let session = parse_trusty_mpm_session(&p).unwrap();
    match session {
        PausedSession::TrustyMpm { tmux_window, .. } => {
            assert_eq!(tmux_window.as_deref(), Some("main:2:@7"));
        }
        _ => panic!("expected TrustyMpm variant"),
    }
}

#[test]
fn parse_missing_tmux_window_is_none() {
    let tmp = TempDir::new().unwrap();
    let p = write_file(
        tmp.path(),
        "session-20260627-100000.md",
        "## Summary\nWork.\n\n## Git Context\nbranch: main",
    );
    let session = parse_trusty_mpm_session(&p).unwrap();
    match session {
        PausedSession::TrustyMpm { tmux_window, .. } => {
            assert!(tmux_window.is_none(), "back-compat: absent section => None");
        }
        _ => panic!("expected TrustyMpm variant"),
    }
}

#[test]
fn render_tmux_window_present_and_omitted() {
    // Present → line renders.
    let with = PausedSession::TrustyMpm {
        path: PathBuf::from("/tmp/session-20260627-100000.md"),
        paused_at: None,
        summary: "Work.".to_string(),
        git_context: None,
        in_progress: None,
        next_steps: None,
        tmux_window: Some("main:2:@7".to_string()),
    };
    let output = render_resume_context(std::slice::from_ref(&with));
    assert!(
        output.contains("**Tmux Window:** main:2:@7"),
        "recorded window should render"
    );

    // None → line omitted.
    let without = PausedSession::TrustyMpm {
        path: PathBuf::from("/tmp/session-20260627-100000.md"),
        paused_at: None,
        summary: "Work.".to_string(),
        git_context: None,
        in_progress: None,
        next_steps: None,
        tmux_window: None,
    };
    let output = render_resume_context(std::slice::from_ref(&without));
    assert!(
        !output.contains("Tmux Window"),
        "absent window must not render a line"
    );
}

// ---------------------------------------------------------------------------
// #5272 — session-snapshot crosstalk
// ---------------------------------------------------------------------------

/// Append a `pause` line attributing `snap` (a path relative to the store root)
/// to `id`.
fn log_pause(sdir: &Path, id: &str, snap: &str, ts: &str) {
    crate::catchup::session_log::append_entry(
        sdir,
        &crate::catchup::session_log::SessionLogEntry {
            session_id: id.to_string(),
            event: "pause".to_string(),
            snapshot: snap.to_string(),
            timestamp: ts.to_string(),
        },
    )
    .unwrap();
}

/// The two ids from the #5272 report: A paused, B never did.
const SESSION_A: &str = "2eb72dca-de08-481b-8dfa-22ab7f81b1f9";
const SESSION_B: &str = "7bd5c27a-475b-41df-9e9f-a6f630801717";

#[test]
fn latest_snapshot_prefers_session_log() {
    let tmp = TempDir::new().unwrap();
    let sdir = tmp.path().join(".trusty-mpm").join("sessions");
    fs::create_dir_all(&sdir).unwrap();

    // Two sessions interleave; each snapshot file exists on disk.
    write_file(&sdir, "session-A.md", "## Summary\nS1 work");
    write_file(&sdir, "session-B.md", "## Summary\nS2 work");
    log_pause(&sdir, "s1", "session-A.md", "t1");
    log_pause(&sdir, "s2", "session-B.md", "t2");

    // s1 resumes its own snapshot even though s2 paused last.
    let got = latest_trusty_mpm_snapshot(tmp.path(), Some("s1")).unwrap();
    assert_eq!(got.file_name().unwrap(), "session-A.md");
    // An explicit request for another session's state is the opt-in, and still
    // succeeds — that is what makes the default refusal a boundary and not a
    // blanket ban.
    let got = latest_trusty_mpm_snapshot(tmp.path(), Some("s2")).unwrap();
    assert_eq!(got.file_name().unwrap(), "session-B.md");
}

/// Why: issue #5272, reproduced exactly. Session `7bd5c27a…` called
/// `session_context_catchup` on a store whose only snapshot,
/// `session-20260809-010155.md`, `sessions-log.jsonl` attributes to
/// `2eb72dca…`. The old chain's "newest pause overall" step handed it over with
/// nothing in the response saying whose it was. Two sessions, one store, a
/// snapshot belonging to A, B resuming: B must get nothing.
/// What: asserts B resolves to `None` while A still resolves to its own file,
/// so the refusal is provably B-specific and not the resolver going dark.
/// Test: itself.
#[test]
fn latest_snapshot_refuses_cross_session_fallback() {
    let tmp = TempDir::new().unwrap();
    let sdir = tmp.path().join(".trusty-mpm").join("sessions");
    fs::create_dir_all(&sdir).unwrap();

    let snap = "session-20260809-010155.md";
    write_file(&sdir, snap, "## Summary\nSession A's work.\n");
    log_pause(&sdir, SESSION_A, snap, "2026-08-09T01:01:55.796934+00:00");

    assert_eq!(
        latest_trusty_mpm_snapshot(tmp.path(), Some(SESSION_B)),
        None,
        "session B has no snapshot of its own and must NOT be handed A's"
    );
    assert_eq!(
        latest_trusty_mpm_snapshot(tmp.path(), Some(SESSION_A))
            .unwrap()
            .file_name()
            .unwrap(),
        snap,
        "A still resolves its own snapshot"
    );
}

/// Why: #5272 — an unidentified caller cannot own anything in a shared store,
/// so "latest overall" is a guess dressed as an answer. Every session-blind
/// route into the native store is closed, not just the one the report hit.
/// What: with a snapshot present and logged, `None` resolves to `None`.
/// Test: itself.
#[test]
fn latest_snapshot_requires_a_session_id() {
    let tmp = TempDir::new().unwrap();
    let sdir = tmp.path().join(".trusty-mpm").join("sessions");
    fs::create_dir_all(&sdir).unwrap();
    write_file(&sdir, "session-20260809-010155.md", "## Summary\nA's work");
    log_pause(&sdir, SESSION_A, "session-20260809-010155.md", "t1");

    assert_eq!(latest_trusty_mpm_snapshot(tmp.path(), None), None);
    assert!(latest_trusty_mpm_snapshot(&tmp.path().join("nope"), Some(SESSION_A)).is_none());
}

/// Why: #5272 back-compat. Flat `session-YYYYMMDD-HHMMSS.md` files at the store
/// root are not migrated — they resolve through `sessions-log.jsonl`, which
/// already attributes them. A flat file with no log line is attributable to
/// nobody and must resolve for nobody, rather than to whichever session asks.
/// What: two flat files, one logged to A and one unlogged; A gets its own, and
/// the orphan is invisible to A and to an unrelated session alike.
/// Test: itself.
#[test]
fn latest_snapshot_reads_legacy_flat_snapshot_via_log() {
    let tmp = TempDir::new().unwrap();
    let sdir = tmp.path().join(".trusty-mpm").join("sessions");
    fs::create_dir_all(&sdir).unwrap();

    write_file(
        &sdir,
        "session-20260807-040441.md",
        "## Summary\nA's legacy",
    );
    // Newer by mtime, and deliberately never logged.
    std::thread::sleep(std::time::Duration::from_millis(10));
    write_file(&sdir, "session-20260807-043031.md", "## Summary\norphan");
    log_pause(&sdir, SESSION_A, "session-20260807-040441.md", "t1");

    assert_eq!(
        latest_trusty_mpm_snapshot(tmp.path(), Some(SESSION_A))
            .unwrap()
            .file_name()
            .unwrap(),
        "session-20260807-040441.md",
        "the logged flat file resolves without migrating it"
    );
    assert_eq!(
        latest_trusty_mpm_snapshot(tmp.path(), Some(SESSION_B)),
        None,
        "the unattributable orphan must not resolve to an arbitrary session"
    );
}

/// Why: #5272 moved pause snapshots into `sessions/<session-id>/`. A digest
/// scan that only read the store root would report an empty catch-up on a
/// project full of them — a regression this change would otherwise introduce.
/// What: one flat snapshot and one nested under a session directory; both reach
/// `find_paused_sessions`.
/// Test: itself.
#[test]
fn find_includes_per_session_directories() {
    let tmp = TempDir::new().unwrap();
    let sdir = tmp.path().join(".trusty-mpm").join("sessions");
    fs::create_dir_all(sdir.join(SESSION_A)).unwrap();
    write_file(&sdir, "session-20260807-040441.md", "## Summary\nflat");
    write_file(
        &sdir.join(SESSION_A),
        "session-20260809-010155.md",
        "## Summary\nnested",
    );

    let found = find_paused_sessions(tmp.path()).unwrap();
    let summaries: Vec<&str> = found
        .iter()
        .filter_map(|s| match s {
            PausedSession::TrustyMpm { summary, .. } => Some(summary.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        summaries.len(),
        2,
        "both layouts are discovered: {summaries:?}"
    );
    assert!(summaries.contains(&"flat"));
    assert!(summaries.contains(&"nested"));
}

#[test]
fn render_empty_returns_no_sessions_message() {
    let output = render_resume_context(&[]);
    assert!(
        output.contains("No paused sessions"),
        "empty renders a notice"
    );
}

// ---------------------------------------------------------------------------
// #5072 — undated snapshots and the watermark filter
// ---------------------------------------------------------------------------

/// Set a file's mtime so ordering assertions don't depend on write order.
fn set_mtime(path: &Path, ts: DateTime<Utc>) {
    let f = fs::File::options().write(true).open(path).unwrap();
    f.set_modified(std::time::SystemTime::from(ts)).unwrap();
}

/// Why: issue #5072 — a hand-written snapshot such as
/// `session-20260730-bounce.md` has a filename that `parse_filename_timestamp`
/// cannot decode, and before the fix `paused_at` was left `None` forever. That
/// made the record undatable, which in turn made the watermark filter admit it
/// unconditionally (see `filter_sessions_since_drops_undatable_session`).
/// What: writes an undated snapshot, stamps a known mtime, and asserts the
/// parser recovers that instant.
/// Test: itself.
#[test]
fn parse_falls_back_to_mtime_when_filename_lacks_timestamp() {
    let tmp = TempDir::new().unwrap();
    let p = write_file(tmp.path(), "session-20260730-bounce.md", "# Hand written\n");
    let expected: DateTime<Utc> = "2026-07-30T14:55:00Z".parse().unwrap();
    set_mtime(&p, expected);

    match parse_trusty_mpm_session(&p).unwrap() {
        PausedSession::TrustyMpm { paused_at, .. } => {
            assert_eq!(
                paused_at,
                Some(expected),
                "an undated filename must fall back to the file's mtime"
            );
        }
        other => panic!("expected TrustyMpm, got {other:?}"),
    }
}

/// Why: issue #5072 — with `paused_at` stuck at `None`, an undated snapshot
/// sorted LAST regardless of how recent it actually was, so the newest-first
/// digest misordered it.
/// What: an undated snapshot with a NEWER mtime than a well-formed dated one
/// must sort first.
/// Test: itself.
#[test]
fn find_orders_undated_snapshot_by_mtime() {
    let tmp = TempDir::new().unwrap();
    let sdir = tmp.path().join(".trusty-mpm").join("sessions");
    fs::create_dir_all(&sdir).unwrap();
    write_file(&sdir, "session-20260101-000000.md", "## Summary\nold");
    let undated = write_file(&sdir, "session-rescue.md", "# Hand written\n");
    set_mtime(&undated, "2026-06-01T00:00:00Z".parse().unwrap());

    let sessions = find_paused_sessions(tmp.path()).unwrap();
    assert_eq!(sessions.len(), 2);
    match &sessions[0] {
        PausedSession::TrustyMpm { path, .. } => {
            assert_eq!(
                path.file_name().unwrap(),
                "session-rescue.md",
                "the undated-but-newer snapshot must sort first"
            );
        }
        other => panic!("expected TrustyMpm, got {other:?}"),
    }
}

/// A genuinely undatable session: no file behind it, so no mtime to fall back
/// on, and an unparseable recorded timestamp. This is the ONLY shape that
/// still yields `sort_key() == None` after #5072 — a session read from disk
/// always has an mtime.
fn undatable_session() -> PausedSession {
    let s = PausedSession::ClaudeMpm {
        session: ClaudeMpmSession {
            paused_at: Some("not-a-timestamp".to_string()),
            source_mtime: None,
            ..Default::default()
        },
    };
    assert!(
        s.sort_key().is_none(),
        "fixture must actually be undatable, or the tests below prove nothing"
    );
    s
}

fn dated_session(ts: &str) -> PausedSession {
    PausedSession::ClaudeMpm {
        session: ClaudeMpmSession {
            paused_at: Some(ts.to_string()),
            ..Default::default()
        },
    }
}

/// Why: issue #5072 — `filter(|s| s.sort_key().is_none_or(|ts| ts > wm))`
/// admitted every session whose timestamp could not be derived, so the ONE
/// record that survived a recent watermark was the undatable one while every
/// genuinely recent snapshot was dropped.
/// What: a session with no derivable timestamp is excluded from a
/// watermark-filtered result, and a session newer than the watermark is kept.
/// Test: itself.
#[test]
fn filter_sessions_since_drops_undatable_session() {
    let wm: DateTime<Utc> = "2026-08-01T00:00:00Z".parse().unwrap();
    let out = filter_sessions_since(
        vec![undatable_session(), dated_session("2026-08-06T21:13:05Z")],
        Some(wm),
    );
    assert_eq!(
        out.kept.len(),
        1,
        "only the datable, newer-than-watermark session survives"
    );
    assert_eq!(
        out.kept[0].sort_key(),
        Some("2026-08-06T21:13:05Z".parse::<DateTime<Utc>>().unwrap())
    );
}

/// Why: the watermark advances past a withheld session and never returns for
/// it, so the count is a receipt the caller must receive — a stderr warning is
/// invisible to an MCP client reading a JSON body (#5072).
/// What: two undatable sessions and one that merely predates the watermark;
/// only the two undatable ones are counted, and one datable session is kept.
/// Test: itself.
#[test]
fn filter_sessions_since_reports_dropped_count() {
    let wm: DateTime<Utc> = "2026-08-01T00:00:00Z".parse().unwrap();
    let out = filter_sessions_since(
        vec![
            undatable_session(),
            undatable_session(),
            dated_session("2026-07-01T00:00:00Z"), // too old — not "undatable"
            dated_session("2026-08-06T21:13:05Z"),
        ],
        Some(wm),
    );
    assert_eq!(out.kept.len(), 1);
    assert_eq!(
        out.dropped_undatable, 2,
        "only undatable exclusions are counted, never merely-too-old ones"
    );
}

/// Why: `full=true` (no watermark) must never drop anything — the fail-closed
/// rule above applies only when there is a watermark to compare against.
/// What: with `None` as the watermark, both sessions survive and nothing is
/// reported as withheld.
/// Test: itself.
#[test]
fn filter_sessions_since_keeps_everything_without_watermark() {
    let out = filter_sessions_since(
        vec![undatable_session(), dated_session("2026-08-06T21:13:05Z")],
        None,
    );
    assert_eq!(out.kept.len(), 2);
    assert_eq!(out.dropped_undatable, 0);
}

/// Why: fail-closed filtering applies to BOTH `PausedSession` variants, but the
/// #5072 mtime rescue initially covered only `TrustyMpm`. A claude-mpm JSON
/// carrying nothing but `session_id` deserialises with `paused_at: None`
/// (`roundtrip_partial_json_uses_defaults` pins that), so it would have gone
/// from "appears in every digest" straight to "silently withheld from all of
/// them" — a regression on the arm the fix did not rescue.
/// What: loads such a file through the real loader and asserts it survives a
/// watermark it postdates, dated by its mtime.
/// Test: itself.
#[test]
fn claude_mpm_session_with_no_paused_at_is_dated_by_mtime() {
    let tmp = TempDir::new().unwrap();
    let sdir = tmp.path().join(".claude-mpm").join("sessions");
    fs::create_dir_all(&sdir).unwrap();
    let p = write_file(
        &sdir,
        "session-legacy.json",
        r#"{"session_id":"legacy-only-id"}"#,
    );
    set_mtime(&p, "2026-08-06T21:13:05Z".parse().unwrap());

    let sessions = find_paused_sessions(tmp.path()).unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(
        sessions[0].sort_key(),
        Some("2026-08-06T21:13:05Z".parse::<DateTime<Utc>>().unwrap()),
        "a claude-mpm session with no paused_at must be dated by its file mtime"
    );

    let wm: DateTime<Utc> = "2026-08-01T00:00:00Z".parse().unwrap();
    let out = filter_sessions_since(sessions, Some(wm));
    assert_eq!(
        out.kept.len(),
        1,
        "and must survive a watermark it postdates"
    );
    assert_eq!(out.dropped_undatable, 0);
}

// ── Code Contract tests (#5724, ADR-0047) ────────────────────────────────────
//
// One test per contract stated in a `# Code Contract` block. These are written
// to fail if the CONTRACT changes, not merely if the implementation does — the
// distinction that matters, because a contract can change under a byte-
// identical signature and no static differ can see it.

/// Why: the canonical instance the whole Code Contract mechanism exists for.
/// `latest_trusty_mpm_snapshot`'s signature is byte-identical from
/// trusty-common 0.24.2 through 0.34.0 while its precondition inverted (#5272):
/// `None` for the session id used to mean "give me the newest snapshot overall"
/// and now means "nothing is attributable to me". cargo-semver-checks and the
/// type differ both report clean across that change.
///
/// This test would FAIL against the pre-#5272 implementation: the store it
/// builds holds two well-formed, logged, recent snapshots, so the old
/// newest-overall fallback returned `Some(session-20260809-020000.md)` where
/// this asserts `None`.
/// What: asserts the `None`-session-id postcondition holds even when the store
/// is full of snapshots that would have matched under the old reading.
/// Test: itself.
#[test]
fn contract_latest_snapshot_none_session_id_is_none() {
    let tmp = TempDir::new().unwrap();
    let sdir = tmp.path().join(".trusty-mpm").join("sessions");
    fs::create_dir_all(&sdir).unwrap();

    // Two attributable snapshots. Under the pre-#5272 "newest pause overall"
    // fallback, an anonymous caller was handed the later of these.
    write_file(&sdir, "session-20260809-010155.md", "## Summary\nA's work");
    log_pause(&sdir, SESSION_A, "session-20260809-010155.md", "t1");
    write_file(&sdir, "session-20260809-020000.md", "## Summary\nB's work");
    log_pause(&sdir, SESSION_B, "session-20260809-020000.md", "t2");

    // Postcondition: `None` in, `None` out — for every project_dir, including
    // one holding snapshots that would have matched before #5272.
    assert_eq!(
        latest_trusty_mpm_snapshot(tmp.path(), None),
        None,
        "an unidentified caller must not be handed another session's snapshot"
    );

    // And the contract's other half: an identified caller still gets its own,
    // so the assertion above is not passing merely because resolution is broken.
    assert_eq!(
        latest_trusty_mpm_snapshot(tmp.path(), Some(SESSION_A)),
        Some(sdir.join("session-20260809-010155.md")),
    );
}

/// Why: #5072 inverted this predicate's treatment of an undatable session, and
/// the inversion silently emptied a real project's digest. The contract states
/// a PARTITION, so the test asserts the partition rather than one example.
/// What: every input session lands in exactly one of three buckets — kept,
/// counted as undatable, or datable-but-not-after-the-watermark — and the three
/// buckets account for the whole input.
/// Test: itself.
#[test]
fn contract_filter_sessions_since_partitions_the_input() {
    let tmp = TempDir::new().unwrap();
    let before = "2026-08-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
    let watermark = "2026-08-05T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
    let after = "2026-08-09T00:00:00Z".parse::<DateTime<Utc>>().unwrap();

    let mk = |paused_at: Option<DateTime<Utc>>| PausedSession::TrustyMpm {
        path: tmp.path().join("s.md"),
        paused_at,
        summary: String::new(),
        git_context: None,
        in_progress: None,
        next_steps: None,
        tmux_window: None,
    };

    let sessions = vec![
        mk(Some(after)),     // kept
        mk(Some(before)),    // datable, does not postdate
        mk(None),            // undatable -> withheld, and counted
        mk(Some(watermark)), // exactly at the watermark: STRICTLY greater, so out
        mk(Some(after)),     // kept
    ];
    let total = sessions.len();

    let out = filter_sessions_since(sessions, Some(watermark));

    assert_eq!(
        out.kept.len(),
        2,
        "only the two strictly-after sessions survive"
    );
    assert_eq!(
        out.dropped_undatable, 1,
        "the undatable session is withheld AND counted"
    );
    // The partition invariant: kept + counted never exceeds the input, and the
    // shortfall is exactly the datable-but-too-old sessions.
    assert!(out.kept.len() + out.dropped_undatable <= total);
    assert_eq!(total - out.kept.len() - out.dropped_undatable, 2);

    // Postcondition: no watermark returns everything, and counts nothing.
    let all = vec![mk(Some(after)), mk(None), mk(Some(before))];
    let out = filter_sessions_since(all, None);
    assert_eq!(out.kept.len(), 3);
    assert_eq!(out.dropped_undatable, 0);
}

/// Why: `sort_key()` returning `None` is what `filter_sessions_since` keys its
/// fail-closed decision on. Before #5072 the caller read `None` as "always
/// include" via `is_none_or`; the contract now states it means EXCLUDED. A test
/// that only checked sorting would not notice that meaning flipping back.
/// What: an undatable session is excluded by a watermark and included without
/// one — the two halves of what `None` now means.
/// Test: itself.
#[test]
fn contract_sort_key_none_means_excluded_not_always_included() {
    let tmp = TempDir::new().unwrap();
    let undatable = PausedSession::TrustyMpm {
        path: tmp.path().join("never-written.md"),
        paused_at: None,
        summary: String::new(),
        git_context: None,
        in_progress: None,
        next_steps: None,
        tmux_window: None,
    };
    assert!(
        undatable.sort_key().is_none(),
        "precondition for the rest of this test"
    );

    // A watermark arbitrarily far in the past still excludes it. Under
    // `is_none_or` this was admitted by EVERY watermark, forever.
    let ancient = "1990-01-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
    let out = filter_sessions_since(vec![undatable], Some(ancient));
    assert!(
        out.kept.is_empty(),
        "`None` must mean excluded, not always-included"
    );
    assert_eq!(out.dropped_undatable, 1);
}
