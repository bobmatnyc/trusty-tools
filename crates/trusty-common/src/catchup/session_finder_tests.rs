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

#[test]
fn latest_snapshot_prefers_session_log() {
    use crate::catchup::session_log::{SessionLogEntry, append_entry};
    let tmp = TempDir::new().unwrap();
    let sdir = tmp.path().join(".trusty-mpm").join("sessions");
    fs::create_dir_all(&sdir).unwrap();

    // Two sessions interleave; each snapshot file exists on disk.
    write_file(&sdir, "session-A.md", "## Summary\nS1 work");
    write_file(&sdir, "session-B.md", "## Summary\nS2 work");
    let mk = |id: &str, snap: &str, ts: &str| SessionLogEntry {
        session_id: id.to_string(),
        event: "pause".to_string(),
        snapshot: snap.to_string(),
        timestamp: ts.to_string(),
    };
    append_entry(&sdir, &mk("s1", "session-A.md", "t1")).unwrap();
    append_entry(&sdir, &mk("s2", "session-B.md", "t2")).unwrap();

    // s1 resumes its own snapshot even though s2 paused last.
    let got = latest_trusty_mpm_snapshot(tmp.path(), Some("s1")).unwrap();
    assert_eq!(got.file_name().unwrap(), "session-A.md");
    // No id → latest overall (s2's).
    let got = latest_trusty_mpm_snapshot(tmp.path(), None).unwrap();
    assert_eq!(got.file_name().unwrap(), "session-B.md");
}

#[test]
fn latest_snapshot_falls_back() {
    let tmp = TempDir::new().unwrap();
    let sdir = tmp.path().join(".trusty-mpm").join("sessions");
    fs::create_dir_all(&sdir).unwrap();
    // No log at all → mtime scan of session-*.md.
    write_file(&sdir, "session-20260101-000000.md", "## Summary\nold");
    std::thread::sleep(std::time::Duration::from_millis(10));
    write_file(&sdir, "session-20260202-000000.md", "## Summary\nnew");
    let got = latest_trusty_mpm_snapshot(tmp.path(), Some("unknown-id")).unwrap();
    assert_eq!(got.file_name().unwrap(), "session-20260202-000000.md");
    // Missing project dir → None.
    assert!(latest_trusty_mpm_snapshot(&tmp.path().join("nope"), None).is_none());
}

#[test]
fn render_empty_returns_no_sessions_message() {
    let output = render_resume_context(&[]);
    assert!(
        output.contains("No paused sessions"),
        "empty renders a notice"
    );
}
