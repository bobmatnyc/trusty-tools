//! Unit tests for `tm session disk`'s row computation, sorting, and rendering
//! (#7313 slice 2).
//!
//! Why: everything worth pinning here is pure — the fold, the order, the class
//! split, the error arm, and the project filter — so none of it needs a daemon,
//! a workspace, or a byte walk. The one thing that does touch the filesystem is
//! the project filter, which gets a temp-dir fixture standing in for a managed
//! workspace root.
//! Test: this file.

use std::path::{Path, PathBuf};

use super::*;

/// Build a survey payload from JSON, through the SAME deserialization the
/// daemon response goes through.
///
/// Why: a hand-built `Survey` literal would prove the fold and nothing about
/// the schema. Going through `serde_json` means a field slice 1 renames breaks
/// these tests, which is the point.
fn survey(value: serde_json::Value) -> Survey {
    serde_json::from_value(value).expect("the fixture must match the survey schema")
}

/// A survey with two attributed sessions and the unattributed bucket, ordered
/// exactly as slice 1's `group_by_session` emits it (bytes descending, `null`
/// last).
fn two_sessions() -> Survey {
    survey(json!({
        "generated_at": "2026-09-10T00:00:00Z",
        "partial": false,
        "by_session": [
            {
                "session_id": "trusty-tools-95",
                "bytes": 30_000_000_000u64,
                "build_dir_bytes": 24_000_000_000u64,
                "worktree_count": 3,
                "tiers": { "stale": 1, "review": 1, "keep": 1, "missing": 0 },
                "worktree_paths": []
            },
            {
                "session_id": "trusty-tools-12",
                "bytes": 10_000_000_000u64,
                "build_dir_bytes": 9_000_000_000u64,
                "worktree_count": 1,
                "tiers": { "stale": 0, "review": 0, "keep": 1, "missing": 0 },
                "worktree_paths": []
            },
            {
                "session_id": null,
                "bytes": 5_000_000_000u64,
                "build_dir_bytes": 4_000_000_000u64,
                "worktree_count": 2,
                "tiers": { "stale": 2, "review": 0, "keep": 0, "missing": 0 },
                "worktree_paths": []
            }
        ],
        "root": { "projects": [] }
    }))
}

/// A survey whose two projects both hold a worktree owned by one session.
fn one_session_across_two_projects() -> Survey {
    survey(json!({
        "generated_at": "2026-09-10T00:00:00Z",
        "partial": false,
        "by_session": [],
        "root": { "projects": [
            {
                "name": "bobmatnyc/trusty-tools",
                "path": "/w/bobmatnyc/trusty-tools",
                "bytes": 40_000_000_000u64,
                "worktrees": [
                    {
                        "path": "/w/bobmatnyc/trusty-tools/.claude/worktrees/a",
                        "tier": "stale",
                        "bytes": 20_000_000_000u64,
                        "build_dir_bytes": 18_000_000_000u64,
                        "owning_session": "trusty-tools-95"
                    },
                    {
                        "path": "/w/bobmatnyc/trusty-tools/.claude/worktrees/b",
                        "tier": "keep",
                        "bytes": 1_000_000_000u64,
                        "build_dir_bytes": 0,
                        "owning_session": "trusty-tools-12"
                    }
                ]
            },
            {
                "name": "bobmatnyc/other",
                "path": "/w/bobmatnyc/other",
                "bytes": 5_000_000_000u64,
                "worktrees": [
                    {
                        "path": "/w/bobmatnyc/other/.claude/worktrees/c",
                        "tier": "review",
                        "bytes": 30_000_000_000u64,
                        "build_dir_bytes": 25_000_000_000u64,
                        "owning_session": "trusty-tools-95"
                    },
                    {
                        "path": "/w/bobmatnyc/other/.claude/worktrees/d",
                        "tier": "missing",
                        "bytes": null,
                        "build_dir_bytes": null,
                        "owning_session": "trusty-tools-95"
                    }
                ]
            }
        ] }
    }))
}

/// The listing preserves slice 1's byte-descending order, unattributed last.
///
/// Why this is the contract and not merely the current behaviour: the order is
/// decided once, in `disk::survey::group_by_session`, so the console and the
/// CLI name the same session first. Re-sorting here would be a second answer.
#[test]
fn sessions_report_sorts_by_bytes_descending() {
    let DiskReport::Sessions { sessions, .. } = sessions_report(&two_sessions(), None) else {
        panic!("the no-argument view must be the sessions view");
    };
    let ids: Vec<Option<&str>> = sessions
        .iter()
        .map(|r| r.session_id.as_deref())
        .collect::<Vec<_>>();
    assert_eq!(
        ids,
        vec![Some("trusty-tools-95"), Some("trusty-tools-12"), None],
        "bytes descending, with the unattributed bucket last"
    );
    let bytes: Vec<u64> = sessions.iter().map(|r| r.bytes).collect();
    assert!(
        bytes.windows(2).all(|w| w[0] >= w[1]),
        "each row must be at least as large as the next: {bytes:?}"
    );
}

/// Every row folds into the total, and `source_bytes` is the remainder.
#[test]
fn sessions_report_totals_every_row() {
    let DiskReport::Sessions {
        sessions, total, ..
    } = sessions_report(&two_sessions(), Some("bobmatnyc/trusty-tools".to_string()))
    else {
        panic!("the no-argument view must be the sessions view");
    };
    assert_eq!(total.bytes, 45_000_000_000);
    assert_eq!(total.build_dir_bytes, 37_000_000_000);
    assert_eq!(
        total.source_bytes, 8_000_000_000,
        "source is what is left after the build directories"
    );
    assert_eq!(total.worktree_count, 6);
    assert_eq!(sessions[0].source_bytes, 6_000_000_000);
}

/// A daemon predating slice 1 answers with no roll-up, and that must read as an
/// empty listing rather than panic.
///
/// Why: `by_session` is `skip_serializing_if = "Option::is_none"`, so an older
/// daemon simply omits the field. Deserializing it as required would turn a
/// version skew into "the payload did not parse".
#[test]
fn a_daemon_without_the_rollup_reports_nothing() {
    let older = survey(json!({
        "generated_at": "2026-09-10T00:00:00Z",
        "partial": false,
        "root": { "projects": [] }
    }));
    let DiskReport::Sessions {
        sessions, total, ..
    } = sessions_report(&older, None)
    else {
        panic!("the no-argument view must be the sessions view");
    };
    assert!(sessions.is_empty());
    assert_eq!(total.bytes, 0);
}

/// A session's breakdown separates the build directories from the rest.
#[test]
fn a_session_report_splits_build_bytes_from_the_rest() {
    let report = session_report(&one_session_across_two_projects(), "trusty-tools-95")
        .expect("the session owns worktrees");
    let DiskReport::Session { classes, total, .. } = report else {
        panic!("an argument must produce the session view");
    };
    assert_eq!(total.bytes, 50_000_000_000);
    assert_eq!(total.build_dir_bytes, 43_000_000_000);
    assert_eq!(total.source_bytes, 7_000_000_000);
    // Largest class first, so the reclaimable majority leads.
    assert_eq!(classes[0].class, "build");
    assert_eq!(classes[0].bytes, 43_000_000_000);
    assert_eq!(classes[1].class, "source");
    assert_eq!(classes[1].bytes, 7_000_000_000);
}

/// A session's worktrees are gathered across EVERY project, bytes descending.
///
/// Why: an isolation worktree, an install/verify throwaway tree, and a
/// `jobs/<id>/` tree can sit under three different repositories while belonging
/// to one dispatching session — the per-project tree cannot express that, which
/// is the whole reason slice 1 added the roll-up.
#[test]
fn a_session_report_spans_projects() {
    let report = session_report(&one_session_across_two_projects(), "trusty-tools-95")
        .expect("the session owns worktrees");
    let DiskReport::Session { worktrees, .. } = report else {
        panic!("an argument must produce the session view");
    };
    let projects: Vec<&str> = worktrees.iter().map(|w| w.project.as_str()).collect();
    assert!(
        projects.contains(&"bobmatnyc/trusty-tools") && projects.contains(&"bobmatnyc/other"),
        "both projects must appear: {projects:?}"
    );
    assert_eq!(
        worktrees[0].path,
        PathBuf::from("/w/bobmatnyc/other/.claude/worktrees/c"),
        "the largest worktree leads, whichever project it is under"
    );
    let bytes: Vec<u64> = worktrees.iter().map(|w| w.bytes.unwrap_or(0)).collect();
    assert!(
        bytes.windows(2).all(|w| w[0] >= w[1]),
        "bytes descending: {bytes:?}"
    );
}

/// An unmeasured worktree is listed and contributes nothing to the totals.
///
/// Why a zero would be wrong: the survey could not measure it, which is not the
/// same claim as "it is empty" — folding it in as zero would make a truncated
/// pass read as a complete one.
#[test]
fn an_unmeasured_worktree_is_listed_and_adds_nothing() {
    let report = session_report(&one_session_across_two_projects(), "trusty-tools-95")
        .expect("the session owns worktrees");
    let DiskReport::Session {
        worktrees, total, ..
    } = report
    else {
        panic!("an argument must produce the session view");
    };
    let unmeasured = worktrees
        .iter()
        .find(|w| w.path.ends_with("d"))
        .expect("the unmeasured worktree must still be listed");
    assert_eq!(unmeasured.bytes, None);
    assert_eq!(unmeasured.source_bytes, None);
    assert_eq!(
        total.worktree_count, 3,
        "it counts as a worktree even though it adds no bytes"
    );
    assert_eq!(total.bytes, 50_000_000_000);
}

/// An id no worktree is attributed to is an error, never an empty report.
///
/// Why: "0 B across 0 worktrees" for a typo is indistinguishable from a session
/// that genuinely holds nothing, and the operator acts on the wrong one.
#[test]
fn an_unknown_session_is_an_error() {
    let err = session_report(&one_session_across_two_projects(), "no-such-session")
        .expect_err("an unattributed id must not produce a report");
    let message = err.to_string();
    assert!(message.contains("no-such-session"), "{message}");
    assert!(
        message.contains("tm session disk"),
        "the error must name the command that lists what IS attributed: {message}"
    );
}

/// A JSON-RPC-level error reaches the operator rather than being swallowed.
#[test]
fn a_jsonrpc_error_is_reported_not_swallowed() {
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "error": { "code": -32603, "message": "daemon is shutting down" }
    });
    let err = survey_from_rpc(&body).expect_err("a JSON-RPC error must not parse as a survey");
    assert!(err.to_string().contains("daemon is shutting down"), "{err}");
}

/// A tool-level `isError` reaches the operator with the tool's own message.
///
/// Why this arm is separate: `dispatch_tool_call` returns HTTP 200 with a
/// JSON-RPC *result* whose `isError` is true — a caller that only checks the
/// `error` member reads the message as a survey and fails on the parse instead,
/// losing the reason.
#[test]
fn a_tool_error_is_reported_not_swallowed() {
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": {
            "content": [{
                "type": "text",
                "text": "disk_survey: unknown `group_by` value `project`"
            }],
            "isError": true
        }
    });
    let err = survey_from_rpc(&body).expect_err("an isError result must not parse as a survey");
    assert!(
        err.to_string().contains("unknown `group_by` value"),
        "{err}"
    );
}

/// A well-formed tool result unwraps into the survey it carries.
#[test]
fn a_survey_payload_round_trips() {
    let payload = json!({
        "generated_at": "2026-09-10T00:00:00Z",
        "partial": true,
        "by_session": [],
        "root": { "projects": [] }
    });
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": {
            "content": [{ "type": "text", "text": payload.to_string() }],
            "isError": false
        }
    });
    let survey = survey_from_rpc(&body).expect("a well-formed result must parse");
    assert_eq!(survey.generated_at, "2026-09-10T00:00:00Z");
    assert!(survey.partial);
}

/// A cwd inside an agent worktree still names the project it belongs to.
///
/// Why: this is where an agent and most operators actually stand, and
/// `<root>/<owner>/<repo>/.claude/worktrees/<x>` must filter to `<owner>/<repo>`
/// rather than to nothing.
#[test]
fn a_worktree_cwd_still_names_its_project() {
    let root = tempfile::tempdir().expect("temp workspace root");
    let cwd = root
        .path()
        .join("bobmatnyc")
        .join("trusty-tools")
        .join(".claude")
        .join("worktrees")
        .join("agent-a82c1d86143c79d5f");
    std::fs::create_dir_all(&cwd).expect("create the worktree fixture");
    assert_eq!(
        project_filter_within(&cwd, root.path()),
        Some("bobmatnyc/trusty-tools".to_string())
    );
    // The project directory itself resolves to the same label.
    assert_eq!(
        project_filter_within(
            &root.path().join("bobmatnyc").join("trusty-tools"),
            root.path()
        ),
        Some("bobmatnyc/trusty-tools".to_string())
    );
}

/// A cwd outside the managed workspace root filters nothing.
///
/// Why not an error: an operator running this from `/tmp` wants the whole
/// fleet's answer, not a refusal — and a filter derived from a path the survey
/// does not cover would silently return an empty listing.
#[test]
fn a_cwd_outside_the_workspace_root_has_no_filter() {
    let root = tempfile::tempdir().expect("temp workspace root");
    let elsewhere = tempfile::tempdir().expect("temp cwd");
    assert_eq!(project_filter_within(elsewhere.path(), root.path()), None);
    // Directly at the root, with no `<owner>/<repo>` beneath it.
    assert_eq!(project_filter_within(root.path(), root.path()), None);
    // One component short of a project.
    assert_eq!(
        project_filter_within(&root.path().join("bobmatnyc"), root.path()),
        None
    );
}

/// The listing renders a header, one row per session, and a total line.
#[test]
fn the_listing_renders_a_total_line() {
    let report = sessions_report(&two_sessions(), Some("bobmatnyc/trusty-tools".to_string()));
    let text = render(&report);
    assert!(text.contains("project bobmatnyc/trusty-tools"), "{text}");
    assert!(text.contains("trusty-tools-95"), "{text}");
    assert!(
        text.contains("(unattributed)"),
        "the null bucket needs a label an operator can read: {text}"
    );
    let total = text
        .lines()
        .find(|l| l.starts_with("TOTAL"))
        .expect("a total line");
    assert!(
        total.contains("41.9 GiB"),
        "the total renders through the shared byte formatter: {total}"
    );
}

/// A UUID session id still leaves the columns aligned.
///
/// Why this is a real failure and not a cosmetic one: slice 1's attribution
/// hands back full 36-character UUIDs, not friendly names, and the first live
/// run of this command shifted every numeric column right on exactly those
/// rows — the table stopped being readable at the one width that matters.
///
/// Fails before the change: the session column was 34 wide.
#[test]
fn a_uuid_session_id_keeps_the_columns_aligned() {
    let uuid = "0b318c84-bae9-4a50-8832-65ed61f8ab22";
    assert_eq!(uuid.len(), 36, "the id width this test exists for");
    let wide = survey(json!({
        "generated_at": "2026-09-10T00:00:00Z",
        "partial": false,
        "by_session": [{
            "session_id": uuid,
            "bytes": 1_000_000_000u64,
            "build_dir_bytes": 400_000_000u64,
            "worktree_count": 1,
            "tiers": { "stale": 0, "review": 0, "keep": 1, "missing": 0 },
            "worktree_paths": []
        }],
        "root": { "projects": [] }
    }));
    let text = render(&sessions_report(&wide, None));
    let lines: Vec<&str> = text.lines().collect();
    // The header, the one row, and the total must put `TOTAL`'s figure in the
    // same column — which only holds while the id fits its field.
    let header = lines
        .iter()
        .find(|l| l.starts_with("SESSION"))
        .expect("a header line");
    let row = lines
        .iter()
        .find(|l| l.starts_with(uuid))
        .expect("the session's own row");
    let total = lines
        .iter()
        .find(|l| l.starts_with("TOTAL"))
        .expect("a total line");
    assert_eq!(
        header.len(),
        row.len(),
        "a UUID must not push the row wider than the header:\n{header}\n{row}"
    );
    assert_eq!(
        header.len(),
        total.len(),
        "and the total must line up with both:\n{header}\n{total}"
    );
}

/// The breakdown names both classes and every worktree.
#[test]
fn the_breakdown_names_every_class() {
    let report = session_report(&one_session_across_two_projects(), "trusty-tools-95")
        .expect("the session owns worktrees");
    let text = render(&report);
    assert!(text.contains("session trusty-tools-95"), "{text}");
    assert!(text.contains("build"), "{text}");
    assert!(text.contains("source"), "{text}");
    assert!(
        text.contains("/w/bobmatnyc/other/.claude/worktrees/d"),
        "an unmeasured worktree is still listed: {text}"
    );
    assert!(
        text.contains('?'),
        "and its figures read as unmeasured, not as zero: {text}"
    );
}

/// A relative cwd is not a workspace path; the filter declines it rather than
/// producing a label the survey cannot match.
#[test]
fn a_relative_cwd_has_no_filter() {
    assert_eq!(
        project_filter_within(Path::new("bobmatnyc/trusty-tools"), Path::new("/w")),
        None
    );
}
