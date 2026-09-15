use super::*;
use trusty_mpm::core::savings::{
    SavingsRow, TECHNIQUE_COMPRESS, TECHNIQUE_INSTRUCTION_COMPRESSION, append_row, now_ts,
};
use trusty_mpm::core::session_record::{KIND_MODEL, KIND_TRANSCRIPT, read_session_record};

fn total(tokens_saved: u64, tokens_before: u64) -> SavingsTotal {
    SavingsTotal {
        tokens_saved,
        tokens_before,
        cost_saved_usd: 0.01,
        rows: 1,
    }
}

/// Append one `compress` row — the per-call technique the segment folds
/// (#7867).
fn write_row(ledger: &Path, session_id: &str, tokens_saved: i64, tokens_before: u64) {
    write_technique_row(
        ledger,
        session_id,
        TECHNIQUE_COMPRESS,
        tokens_saved,
        tokens_before,
    );
}

fn write_technique_row(
    ledger: &Path,
    session_id: &str,
    technique: &str,
    tokens_saved: i64,
    tokens_before: u64,
) {
    append_row(
        ledger,
        &SavingsRow {
            ts: now_ts(),
            session_id: session_id.to_string(),
            technique: technique.to_string(),
            tokens_saved,
            tokens_before,
            cost_saved_usd: 0.18,
            basis: "fixture".to_string(),
            model_source: "launch-config".to_string(),
        },
    )
    .expect("append");
}

/// Why (#7867): the owner ruled the segment measures rtk/shunt-style
/// tool-output compression, credited per call. An instruction-fold row is one
/// launch-time comparison of the compiled prompt against its sources — a
/// different measurement on a different clock — and before this fix it moved
/// the figure on its own, so a project whose prompt happened to shrink read as
/// a session that compressed tool output.
/// What: writes ONLY an instruction-compression row for the session and asserts
/// the render is the explicit empty state, not a percent.
/// Test: itself.
#[test]
fn an_instruction_compression_row_alone_renders_the_empty_state() {
    let dir = tempfile::tempdir().expect("temp dir");
    let ledger = dir.path().join("savings.jsonl");
    write_technique_row(
        &ledger,
        "sess-1",
        TECHNIQUE_INSTRUCTION_COMPRESSION,
        12_000,
        60_000,
    );

    assert_eq!(
        savings_segment_at(&ledger, "sess-1").as_deref(),
        Some(EMPTY_STATE),
        "an instruction-fold row is not a tool-output compression measurement"
    );
}

/// Why (#7867): the other half of the ruling — a Bash call compressed a moment
/// ago must move the figure on the next render, with no batching and no
/// once-per-session gate between the append and the fold.
/// What: renders the empty state for a session with no rows, appends one
/// `compress` row, and asserts the very next render carries a percent.
/// Test: itself.
#[test]
fn one_compress_row_moves_the_segment() {
    let dir = tempfile::tempdir().expect("temp dir");
    let ledger = dir.path().join("savings.jsonl");
    write_technique_row(
        &ledger,
        "sess-1",
        TECHNIQUE_INSTRUCTION_COMPRESSION,
        12_000,
        60_000,
    );
    assert_eq!(
        savings_segment_at(&ledger, "sess-1").as_deref(),
        Some(EMPTY_STATE)
    );

    write_row(&ledger, "sess-1", 12_000, 60_000);

    assert_eq!(
        savings_segment_at(&ledger, "sess-1").as_deref(),
        Some("\u{1f4b8}20%/20%"),
        "the compress row must be visible on the next render"
    );
}

/// Why (#8063): a session with one recorded row renders that row's own
/// reduction on both sides — the badge never goes half-blank, and the two
/// halves agree when there is only one measurement to report.
/// Test: itself.
#[test]
fn savings_segment_renders_a_percent() {
    assert_eq!(
        render_savings_segment(&total(1, 3), &[33]).as_deref(),
        Some("\u{1f4b8}33%/33%")
    );
    assert_eq!(
        render_savings_segment(&total(5_000, 20_000), &[25]).as_deref(),
        Some("\u{1f4b8}25%/25%")
    );
}

/// Why (#7074, re-scoped by #8063): the mean renders beside the latest row's
/// figure, not instead of it. A render that dropped either half would still
/// look like a savings segment.
/// Test: itself.
#[test]
fn savings_segment_renders_the_average_beside_the_latest_row() {
    assert_eq!(
        render_savings_segment(&total(5_000, 20_000), &[25, 29, 33]).as_deref(),
        Some("\u{1f4b8}33%/29%"),
        "the newest row is the left figure; the mean of all three is the right"
    );
}

/// The reported defect, at the renderer (#8063).
///
/// Why: the owner read `💸1%` while `tm compress` was cutting individual tool
/// outputs by ~19 %, because the left figure was a whole-session share. The
/// owner's 2026-09-15 ruling makes it the LATEST row's own reduction. Two rows
/// whose session total differs from both of them is what tells the two rules
/// apart: the session total here is 21 % (15_000 / 70_000) and appears
/// nowhere in the rendered segment.
/// Test: itself, plus
/// `savings_segment_renders_the_latest_row_not_the_session_total` end to end.
#[test]
fn render_uses_the_latest_row_not_the_session_total() {
    assert_eq!(
        render_savings_segment(&total(15_000, 70_000), &[20, 30]).as_deref(),
        Some("\u{1f4b8}30%/25%"),
        "21% — the session total — must not appear on either side"
    );
}

/// The reported defect, end to end (#8063).
///
/// Why: the renderer test above is fed hand-built percents, so it cannot catch
/// a fold that hands the renderer the wrong ones. This one starts at the
/// ledger: two `compress` rows for one session, 20 % then 30 %, whose pooled
/// session ratio is 21 %. Against the pre-#8063 fold this rendered
/// `💸21%/21%`.
/// What: appends both rows in order and asserts the rendered segment carries
/// the newest row's 30 % on the left and the mean 25 % on the right.
/// Test: itself.
#[test]
fn savings_segment_renders_the_latest_row_not_the_session_total() {
    let dir = tempfile::tempdir().expect("temp dir");
    let ledger = savings_log_in(dir.path());
    write_row(&ledger, "sess-1", 12_000, 60_000); // 20 %
    write_row(&ledger, "sess-1", 3_000, 10_000); // 30 %, and the newest

    assert_eq!(
        savings_segment_at(&ledger, "sess-1").as_deref(),
        Some("\u{1f4b8}30%/25%"),
        "the newest row's 30% must be the left figure, not the 21% session total"
    );
}

/// Why: a zero fold — no rows at all, or rows that summed to nothing — must
/// omit the segment, not render a placeholder.
/// Test: itself.
#[test]
fn savings_segment_is_absent_on_a_zero_fold() {
    assert_eq!(render_savings_segment(&SavingsTotal::default(), &[]), None);
    assert_eq!(
        render_savings_segment(
            &SavingsTotal {
                tokens_saved: 0,
                tokens_before: 0,
                cost_saved_usd: 0.0,
                rows: 3,
            },
            &[40],
        ),
        None,
        "a zero fold renders nothing, whatever percents are in hand"
    );
}

/// Why (#7179): a fold with `tokens_saved > 0` whose every accepted row
/// predates #7179's `tokens_before` has no denominator to divide by — it
/// yields no row percent at all — and must omit the segment rather than
/// fabricate one against nothing.
/// Test: itself.
#[test]
fn savings_segment_is_absent_without_a_percent_denominator() {
    assert_eq!(
        render_savings_segment(
            &SavingsTotal {
                tokens_saved: 4_000,
                tokens_before: 0,
                cost_saved_usd: 0.01,
                rows: 1,
            },
            &[],
        ),
        None
    );
}

/// Why (#7179): the one output this segment may never produce, asserted
/// from the ledger rather than inferred from the format test. A sub-0.5 %
/// row must round UP to the smallest displayable percent, on both sides
/// since #8063 — a rendered `0%` would state a measurement that was never
/// made.
/// Test: itself.
#[test]
fn savings_segment_never_renders_zero_percent() {
    for (tokens_saved, tokens_before) in [(1_i64, 200_u64), (12_000, 12_000_100), (5, 1_000)] {
        let dir = tempfile::tempdir().expect("temp dir");
        let ledger = savings_log_in(dir.path());
        write_row(&ledger, "sess-1", tokens_saved, tokens_before);
        assert_eq!(
            savings_segment_at(&ledger, "sess-1").as_deref(),
            Some("\u{1f4b8}1%/1%"),
            "a tiny but real saving rounds up to 1%, never down to 0% \
             (tokens_saved={tokens_saved}, tokens_before={tokens_before})"
        );
    }
}

/// Why (#6958, empty state since #7617): the ledger is normally absent — no
/// producer has run — and that must cost the status bar nothing. It renders
/// the explicit empty state rather than vanishing, so "no rows yet" and
/// "the segment broke" are not the same picture.
/// Test: itself.
#[test]
fn savings_segment_is_absent_when_the_ledger_is_missing() {
    let dir = tempfile::tempdir().expect("temp dir");
    let ledger = dir.path().join("usage").join("savings.jsonl");
    assert_eq!(
        savings_segment_at(&ledger, "sess-1").as_deref(),
        Some(EMPTY_STATE)
    );
}

/// Why (#7074, acceptance criterion b): an EMPTY ledger must render neither
/// the per-session figure nor the average — a file that exists but holds no
/// row is a different code path from a file that does not exist. Since
/// #7617 both render the empty state, and neither renders a figure.
/// Test: itself.
#[test]
fn savings_segment_is_absent_on_an_empty_ledger() {
    let dir = tempfile::tempdir().expect("temp dir");
    let ledger = savings_log_in(dir.path());
    std::fs::create_dir_all(ledger.parent().expect("parent")).expect("mkdir");
    std::fs::write(&ledger, "").expect("write empty ledger");
    assert_eq!(
        savings_segment_at(&ledger, "sess-1").as_deref(),
        Some(EMPTY_STATE)
    );
}

/// Why: proves the whole path — append a row, fold it back for that session
/// id, and render — without touching the operator's real root. With one row
/// recorded, the mean IS that row's figure, so both sides read the same
/// (#8063).
/// Test: itself.
#[test]
fn savings_segment_reads_the_ledger_under_an_explicit_root() {
    let dir = tempfile::tempdir().expect("temp dir");
    let ledger = savings_log_in(dir.path());
    write_row(&ledger, "sess-1", 12_000, 60_000);
    assert_eq!(
        savings_segment_at(&ledger, "sess-1").as_deref(),
        Some("\u{1f4b8}20%/20%")
    );
    // A different session's bar reads no FIGURE from the same file — and
    // says so explicitly rather than vanishing (#7617).
    assert_eq!(
        savings_segment_at(&ledger, "sess-2").as_deref(),
        Some(EMPTY_STATE)
    );
}

/// A restart's new session id folds the managed session's earlier rows.
///
/// Why (#7617): this is the reported disappearance, reproduced. Managed
/// session 0b318c84 carried three Claude ids on 2026-09-12 (3544c9e5 →
/// 63589e53 → f3def033) and the segment went dark after each restart until
/// the new id had earned rows of its own. The rows were never lost; the
/// fold simply could not reach them.
/// Test: itself.
#[test]
fn savings_segment_folds_a_sibling_session_id() {
    let dir = tempfile::tempdir().expect("temp dir");
    let ledger = savings_log_in(dir.path());
    write_row(&ledger, "claude-old", 12_000, 60_000);
    trusty_mpm::core::session_links::record_link(dir.path(), "managed-1", "claude-old");
    trusty_mpm::core::session_links::record_link(dir.path(), "managed-1", "claude-new");

    assert_eq!(
        savings_segment_at_in(dir.path(), &ledger, "claude-new").as_deref(),
        Some("\u{1f4b8}20%/20%"),
        "a restart's new id must fold its managed session's earlier rows"
    );
}

/// Why (#7617): the sibling fold is a FALLBACK, never a substitution. A
/// session with rows of its own must report those, so a long-running
/// session is never shown a superseded id's figure.
/// Test: itself.
#[test]
fn savings_segment_prefers_this_sessions_own_rows() {
    let dir = tempfile::tempdir().expect("temp dir");
    let ledger = savings_log_in(dir.path());
    write_row(&ledger, "claude-old", 12_000, 60_000);
    write_row(&ledger, "claude-new", 30_000, 60_000);
    trusty_mpm::core::session_links::record_link(dir.path(), "managed-1", "claude-old");
    trusty_mpm::core::session_links::record_link(dir.path(), "managed-1", "claude-new");

    assert_eq!(
        savings_segment_at_in(dir.path(), &ledger, "claude-new").as_deref(),
        Some("\u{1f4b8}50%/50%"),
        "this session's own 50% must win over the sibling's 20%, on both sides"
    );
}

/// Why (#7617, closure condition 3): the segment never omits itself
/// silently. An unknown id, an empty id and an unreadable ledger all render
/// a mark that claims no number, so a disappearance reads differently from
/// a zero.
/// Test: itself.
#[test]
fn savings_segment_renders_the_empty_state_on_a_zero_fold() {
    let dir = tempfile::tempdir().expect("temp dir");
    let ledger = savings_log_in(dir.path());
    write_row(&ledger, "claude-old", 12_000, 60_000);

    for id in ["never-seen", ""] {
        assert_eq!(
            savings_segment_at_in(dir.path(), &ledger, id).as_deref(),
            Some(EMPTY_STATE),
            "session id {id:?} must render the explicit empty state"
        );
    }
    assert_ne!(
        EMPTY_STATE, "\u{1f4b8}0%",
        "the empty state must never be mistakable for a measured zero"
    );
}

/// Why (#8063): the mean is scoped to THIS session's rows. The 2026-09-15
/// ruling reads "the average across all savings rows recorded in this
/// session", so another session's 90 % must not raise a bar the operator
/// reads as their own. Three sessions on one ledger, and only sess-a's two
/// rows reach sess-a's bar — 10 and 30 average to 20, with the newer 30 on
/// the left.
/// Test: itself.
#[test]
fn savings_segment_averages_this_sessions_own_rows() {
    let dir = tempfile::tempdir().expect("temp dir");
    let ledger = savings_log_in(dir.path());
    write_row(&ledger, "sess-a", 100, 1_000); // 10 %
    write_row(&ledger, "sess-b", 900, 1_000); // 90 %, another session
    write_row(&ledger, "sess-a", 3_000, 10_000); // 30 %, and sess-a's newest

    assert_eq!(
        savings_segment_at(&ledger, "sess-a").as_deref(),
        Some("\u{1f4b8}30%/20%"),
        "sess-b's 90% must reach neither figure"
    );
}

/// Why (#7074, acceptance criterion e): the average is derived at read
/// time. A future implementation that cached it into a rollup file would
/// reintroduce the second writer the 2026-07-29 owner ruling forbids, and
/// the two surfaces could then drift. Asserting the directory listing is
/// byte-identical after a render is what catches that.
/// Test: itself.
#[test]
fn rendering_writes_nothing_under_the_usage_directory() {
    let dir = tempfile::tempdir().expect("temp dir");
    let ledger = savings_log_in(dir.path());
    write_row(&ledger, "sess-a", 100, 1_000);
    write_row(&ledger, "sess-b", 300, 1_000);

    let usage_dir = ledger.parent().expect("usage dir").to_path_buf();
    let listing = |dir: &Path| -> Vec<(String, u64)> {
        let mut entries: Vec<(String, u64)> = std::fs::read_dir(dir)
            .expect("read usage dir")
            .map(|entry| {
                let entry = entry.expect("entry");
                let len = entry.metadata().expect("metadata").len();
                (entry.file_name().to_string_lossy().into_owned(), len)
            })
            .collect();
        entries.sort();
        entries
    };

    let before = listing(&usage_dir);
    assert!(
        savings_segment_at(&ledger, "sess-a").is_some(),
        "the fixture must render, or this test proves nothing"
    );
    assert_eq!(
        listing(&usage_dir),
        before,
        "rendering must add or grow no file under usage/"
    );
}

/// Why: before Claude Code assigns a session id there is nothing to fold.
/// Since #7617 that renders the explicit empty state rather than omitting
/// the segment — an operator watching the bar go blank cannot otherwise
/// tell "no id yet" from "the segment broke", which is the whole reported
/// symptom. It still fabricates no figure.
/// Test: itself.
#[test]
fn savings_segment_probe_is_absent_without_a_session_id() {
    assert_eq!(savings_segment_probe("").as_deref(), Some(EMPTY_STATE));
}

/// A Claude-config-shaped directory holding one real transcript file.
///
/// Why: `contained_transcript_path` canonicalizes both sides, and
/// `canonicalize` refuses a path that does not exist — so a fixture that
/// only names a file proves nothing.
fn transcript_under(config_dir: &Path, session_id: &str) -> PathBuf {
    let projects = config_dir.join("projects").join("slug");
    std::fs::create_dir_all(&projects).expect("mkdir");
    let transcript = projects.join(format!("{session_id}.jsonl"));
    std::fs::write(&transcript, "{}\n").expect("write transcript");
    transcript
}

fn recorded_transcript(root: &Path, session_id: &str) -> Option<String> {
    read_session_record(root, KIND_TRANSCRIPT, session_id)
}

/// Why (#7250): the payload names the file a LATER `tm` process opens, so a
/// path outside the session's Claude config directory must never reach the
/// store. The model, which carries no path, is still recorded.
/// Test: itself.
#[test]
fn a_transcript_path_outside_the_config_dir_is_not_recorded() {
    let root = tempfile::tempdir().expect("temp dir");
    let config = tempfile::tempdir().expect("temp dir");
    let elsewhere = tempfile::tempdir().expect("temp dir");
    let outside = elsewhere.path().join("stolen.jsonl");
    std::fs::write(&outside, "{}\n").expect("write");

    record_session_facts_at(
        root.path(),
        Some(config.path()),
        "sess-1",
        "claude-opus-4-1",
        "/etc/passwd",
        "",
    );
    assert_eq!(recorded_transcript(root.path(), "sess-1"), None);

    record_session_facts_at(
        root.path(),
        Some(config.path()),
        "sess-1",
        "claude-opus-4-1",
        &outside.to_string_lossy(),
        "",
    );
    assert_eq!(recorded_transcript(root.path(), "sess-1"), None);
    assert_eq!(
        read_session_record(root.path(), KIND_MODEL, "sess-1").as_deref(),
        Some("claude-opus-4-1"),
        "a rejected transcript path must not cost the model record"
    );
}

/// Why (#7250): a `..` component walks out of the config directory while
/// still looking like it starts inside it.
/// Test: itself.
#[test]
fn a_traversing_transcript_path_is_not_recorded() {
    let root = tempfile::tempdir().expect("temp dir");
    let config = tempfile::tempdir().expect("temp dir");
    transcript_under(config.path(), "sess-1");
    let traversing = config
        .path()
        .join("projects")
        .join("..")
        .join("..")
        .join("etc")
        .join("passwd");

    record_session_facts_at(
        root.path(),
        Some(config.path()),
        "sess-1",
        "claude-opus-4-1",
        &traversing.to_string_lossy(),
        "",
    );
    assert_eq!(recorded_transcript(root.path(), "sess-1"), None);
}

/// Why (#7250, critic round 2): with no `CLAUDE_CONFIG_DIR` and no home
/// directory there is no directory to contain the path, and the earlier
/// `FrameworkPaths::default()` fallback resolved to `"."` — which
/// canonicalizes to the working directory, quietly re-scoping containment
/// to `<cwd>/.claude` instead of refusing. `claude_config_dir` now answers
/// `None` there, and nothing lands in the store. This bin target may not
/// write `HOME` (#5544), so the `None` arrives as an argument.
/// Test: itself.
#[test]
fn a_transcript_path_is_not_recorded_without_a_config_dir() {
    let root = tempfile::tempdir().expect("temp dir");
    let config = tempfile::tempdir().expect("temp dir");
    let transcript = transcript_under(config.path(), "sess-1");

    record_session_facts_at(
        root.path(),
        None,
        "sess-1",
        "claude-opus-4-1",
        &transcript.to_string_lossy(),
        "",
    );
    assert_eq!(
        recorded_transcript(root.path(), "sess-1"),
        None,
        "with no config directory there is nothing to contain the path"
    );
    assert_eq!(
        read_session_record(root.path(), KIND_MODEL, "sess-1").as_deref(),
        Some("claude-opus-4-1"),
        "the model carries no path, so it is still recorded"
    );
}

/// Why (#7250): the screen must still accept the ordinary payload, or the
/// commit footer silently loses its token counts. What lands is the
/// canonical path, which on macOS differs from the temp directory's own
/// spelling.
/// Test: itself.
#[test]
fn a_transcript_path_under_the_config_dir_is_recorded() {
    let root = tempfile::tempdir().expect("temp dir");
    let config = tempfile::tempdir().expect("temp dir");
    let transcript = transcript_under(config.path(), "sess-1");

    record_session_facts_at(
        root.path(),
        Some(config.path()),
        "sess-1",
        "claude-opus-4-1",
        &transcript.to_string_lossy(),
        "",
    );
    assert_eq!(
        recorded_transcript(root.path(), "sess-1"),
        Some(
            transcript
                .canonicalize()
                .expect("canonicalize")
                .to_string_lossy()
                .into_owned()
        )
    );
}

/// Why (#7424): the render that first sees an assistant turn is the only
/// process holding the session id, the screened transcript path and the
/// working directory at once, so it is where the startup reading is taken.
/// A render that takes it must also not take it twice.
/// Test: itself.
#[test]
fn the_first_render_records_the_startup_context() {
    let root = tempfile::tempdir().expect("temp dir");
    let config = tempfile::tempdir().expect("temp dir");
    let project = tempfile::tempdir().expect("temp dir");
    let transcript = transcript_under(config.path(), "sess-1");
    std::fs::write(
        &transcript,
        "{\"type\":\"assistant\",\"message\":{\"id\":\"m1\",\"usage\":{\"input_tokens\":4,\
         \"cache_creation_input_tokens\":74685,\"cache_read_input_tokens\":27297,\
         \"output_tokens\":9}}}\n",
    )
    .expect("write transcript");

    record_session_facts_at(
        root.path(),
        Some(config.path()),
        "sess-1",
        "claude-opus-4-1",
        &transcript.to_string_lossy(),
        &project.path().to_string_lossy(),
    );

    let stored = trusty_mpm::core::startup_context::read_startup_context(root.path(), "sess-1")
        .expect("a startup reading");
    assert_eq!(stored.tokens, 101_986);
    // A render with no cwd in its payload records nothing new, and the
    // reading already taken is never revised.
    record_session_facts_at(
        root.path(),
        Some(config.path()),
        "sess-1",
        "claude-opus-4-1",
        &transcript.to_string_lossy(),
        "",
    );
    assert_eq!(
        trusty_mpm::core::startup_context::read_startup_context(root.path(), "sess-1")
            .expect("still there")
            .tokens,
        101_986
    );
}
