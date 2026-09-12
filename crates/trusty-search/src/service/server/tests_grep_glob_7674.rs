//! #7674 — the grep `glob` parameter must behave identically for two sibling
//! directories, and a glob that selects no file must say so.
//!
//! Reproduces lookup L3 of `docs/research/input-token-optimization-spike-2026-09-12.md`:
//! `glob = "crates/trusty-mpm/src/bin/tm/commands/statusline/*.rs"` returned
//! `{matches: [], total: 0}` while the identically shaped
//! `crates/trusty-mpm/src/core/session_launch/*.rs` returned every file, so the
//! caller could not tell a real zero from a broken filter. The fixture below
//! mirrors those two sibling paths exactly.

use super::files::{global_grep_handler, grep_handler};
use super::tests_grep::stage_grep_index;
use super::*;
use crate::service::grep::GrepRequest;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;

/// The sibling that L4 globbed successfully.
const WORKING_SIBLING: &str = "crates/trusty-mpm/src/core/session_launch/settings.rs";
/// The sibling that L3 globbed to zero.
const BROKEN_SIBLING: &str = "crates/trusty-mpm/src/bin/tm/commands/statusline/savings.rs";
/// The literal both siblings contain.
const NEEDLE: &str = "render_savings_segment";

/// Build a request with everything defaulted but `pattern` and `glob`.
fn req(glob: Option<&str>) -> GrepRequest {
    let mut r: GrepRequest = serde_json::from_value(serde_json::json!({ "pattern": NEEDLE }))
        .expect("default grep request");
    r.glob = glob.map(str::to_string);
    r
}

/// Stage both sibling directories, each holding one file with the needle.
async fn both_siblings() -> (Arc<SearchAppState>, tempfile::TempDir) {
    let body = format!("// header\nfn {NEEDLE}() {{}}\n");
    let (state, _id, tmp) = stage_grep_index(&[
        (WORKING_SIBLING, body.as_str()),
        (BROKEN_SIBLING, body.as_str()),
    ])
    .await;
    (state, tmp)
}

/// Run a glob against the staged index and return the response.
async fn grep(
    state: Arc<SearchAppState>,
    glob: Option<&str>,
) -> crate::service::grep::GrepResponse {
    let Json(resp) = grep_handler(State(state), Path("grep-test".to_string()), Json(req(glob)))
        .await
        .expect("200");
    resp
}

/// The files a response reports, sorted, for order-independent comparison.
fn files(resp: &crate::service::grep::GrepResponse) -> Vec<String> {
    let mut f: Vec<String> = resp.matches.iter().map(|m| m.file.clone()).collect();
    f.sort();
    f
}

/// The L3 core: the same glob SHAPE must select the same files in either
/// sibling directory. Before #7674 this held; it is pinned so the normalization
/// work cannot regress the case that already worked.
#[tokio::test]
async fn sibling_directories_answer_an_identically_shaped_glob_identically() {
    let (state, _tmp) = both_siblings().await;

    let working = grep(
        state.clone(),
        Some("crates/trusty-mpm/src/core/session_launch/*.rs"),
    )
    .await;
    let broken = grep(
        state,
        Some("crates/trusty-mpm/src/bin/tm/commands/statusline/*.rs"),
    )
    .await;

    assert_eq!(files(&working), vec![WORKING_SIBLING.to_string()]);
    assert_eq!(
        files(&broken),
        vec![BROKEN_SIBLING.to_string()],
        "the statusline sibling must answer the same shape the session_launch sibling answers"
    );
}

/// Every glob shape #7674 enumerates, against one known corpus, with the
/// file set each must select. `src/**/*.rs` is anchored at the index root by
/// design (ripgrep's rule for a glob containing `/`), so selecting nothing is
/// CORRECT — what was wrong is that it could not be told apart from a miss.
#[tokio::test]
async fn every_glob_shape_selects_the_documented_file_set() {
    let (state, tmp) = both_siblings().await;
    let absolute = tmp.path().join(BROKEN_SIBLING);
    let absolute = absolute.to_string_lossy().to_string();

    let cases: Vec<(&str, Vec<&str>)> = vec![
        // Recursive, extension-scoped: both siblings.
        ("**/*.rs", vec![BROKEN_SIBLING, WORKING_SIBLING]),
        // Crate-scoped recursive: both siblings.
        (
            "crates/trusty-mpm/**",
            vec![BROKEN_SIBLING, WORKING_SIBLING],
        ),
        // Anchored mid-path: correctly empty — no file sits at `src/` in the
        // index root. The `meta` note is what makes that legible.
        ("src/**/*.rs", vec![]),
        // Basename-only: `rg -g savings.rs` matches at any depth, and so must
        // this — it returned nothing before #7674.
        ("savings.rs", vec![BROKEN_SIBLING]),
        // Absolute, exactly as a `search` result reports `file` — it returned
        // nothing before #7674.
        (absolute.as_str(), vec![BROKEN_SIBLING]),
    ];

    for (glob, expected) in cases {
        let resp = grep(state.clone(), Some(glob)).await;
        let mut want: Vec<String> = expected.iter().map(|s| s.to_string()).collect();
        want.sort();
        assert_eq!(files(&resp), want, "glob {glob} selected the wrong files");
    }
}

/// A basename-only glob reaches a file at any depth (`rg -g` parity).
#[tokio::test]
async fn grep_honours_a_basename_only_glob_at_any_depth() {
    let (state, _tmp) = both_siblings().await;
    let resp = grep(state, Some("savings.rs")).await;
    assert_eq!(files(&resp), vec![BROKEN_SIBLING.to_string()]);
    let meta = resp.meta.expect("a glob request carries meta");
    assert_eq!(meta.glob, "savings.rs");
    assert_eq!(meta.glob_normalized, "**/savings.rs");
    assert_eq!(meta.glob_matched_files, 1);
    assert!(
        meta.note.is_none(),
        "a glob that selected a file owes no note"
    );
}

/// An absolute glob — the spelling `search`/`search_lexical` report in their
/// own `file` field — works verbatim when pasted into `grep`'s `glob`.
#[tokio::test]
async fn grep_honours_an_absolute_glob_as_search_reports_it() {
    let (state, tmp) = both_siblings().await;
    let absolute = tmp
        .path()
        .join(BROKEN_SIBLING)
        .to_string_lossy()
        .to_string();
    let resp = grep(state, Some(&absolute)).await;
    assert_eq!(
        files(&resp),
        vec![BROKEN_SIBLING.to_string()],
        "an absolute glob must resolve against the index root"
    );
}

/// The L3 symptom itself: a glob naming a directory that is real on disk but
/// absent from the corpus must report `glob_matched_files: 0` and say in words
/// that the filter, not the pattern, produced the empty result.
#[tokio::test]
async fn an_unindexed_directory_reports_glob_matched_files_zero_with_a_note() {
    // Only the working sibling is indexed; the statusline sibling exists on
    // disk but was never chunked — exactly the state L3 hit.
    let body = format!("fn {NEEDLE}() {{}}\n");
    let (state, _id, tmp) = stage_grep_index(&[(WORKING_SIBLING, body.as_str())]).await;
    let on_disk = tmp.path().join(BROKEN_SIBLING);
    std::fs::create_dir_all(on_disk.parent().expect("parent")).expect("mkdirs");
    std::fs::write(&on_disk, &body).expect("write the unindexed sibling");

    let resp = grep(
        state,
        Some("crates/trusty-mpm/src/bin/tm/commands/statusline/*.rs"),
    )
    .await;

    assert_eq!(resp.total, 0, "the file is not in the corpus");
    let meta = resp
        .meta
        .expect("a zero result under a glob must carry the diagnostic");
    assert_eq!(meta.glob_matched_files, 0);
    assert_eq!(meta.corpus_files, 1);
    let note = meta.note.expect("zero selected files must carry a note");
    assert!(
        note.contains("selected 0 of 1 indexed files"),
        "note must name the counts, was: {note}"
    );
    assert!(
        note.contains("does NOT mean the pattern is absent"),
        "note must disclaim the empty match array, was: {note}"
    );
}

/// An anchored mid-path glob is empty by ripgrep's own rule, and the note is
/// what tells the caller that rather than leaving a bare `[]`.
#[tokio::test]
async fn grep_reports_a_glob_that_selected_no_files() {
    let (state, _tmp) = both_siblings().await;
    let resp = grep(state, Some("src/**/*.rs")).await;
    assert_eq!(resp.total, 0);
    let meta = resp.meta.expect("meta is owed whenever a glob is supplied");
    assert_eq!(
        meta.glob_normalized, "src/**/*.rs",
        "anchored, not rewritten"
    );
    assert_eq!(meta.glob_matched_files, 0);
    assert_eq!(meta.corpus_files, 2);
    assert!(meta.note.is_some());
}

/// Fail-Open Check: a glob that will not parse is a `400`, never an empty
/// result set wearing a diagnostic.
#[tokio::test]
async fn a_glob_that_fails_to_parse_is_an_error_not_an_empty_result() {
    let (state, _tmp) = both_siblings().await;
    let err = grep_handler(
        State(state),
        Path("grep-test".to_string()),
        Json(req(Some("crates/[unterminated"))),
    )
    .await
    .expect_err("an unparseable glob must be rejected");
    assert_eq!(err.0, StatusCode::BAD_REQUEST);
    let msg = err.1 .0["error"]
        .as_str()
        .expect("error string")
        .to_string();
    assert!(
        msg.contains("invalid glob pattern"),
        "the 400 must name the glob as the cause, was: {msg}"
    );
}

/// A request with no glob carries no diagnostic — `meta` is not noise on the
/// common path.
#[tokio::test]
async fn a_request_without_a_glob_carries_no_meta() {
    let (state, _tmp) = both_siblings().await;
    let resp = grep(state, None).await;
    assert_eq!(resp.total, 2);
    assert!(resp.meta.is_none());
}

/// The global fan-out reports the same diagnostic as the index-scoped path.
#[tokio::test]
async fn global_grep_reports_the_glob_diagnostic_too() {
    let (state, _tmp) = both_siblings().await;
    let Json(resp) = global_grep_handler(State(state), Json(req(Some("src/**/*.rs"))))
        .await
        .expect("200");
    assert_eq!(resp.total, 0);
    let meta = resp.meta.expect("the fan-out owes the same diagnostic");
    assert_eq!(meta.glob_matched_files, 0);
    assert_eq!(meta.corpus_files, 2);
    assert!(meta.note.is_some());
}
