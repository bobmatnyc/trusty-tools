//! Tests for the tool-output compression savings producer.
//!
//! Why: the producer's contract is "claim only what the compression genuinely
//! avoided". The passthrough case is the one that matters most: `tm compress`
//! returns short output unchanged, and a row written for such a run would carry
//! a `tokens_before` into the fold while saving nothing — dragging the `💸`
//! segment's per-session percentage, and therefore its average, down.
//! What: the row builder is driven with hand-supplied byte counts and an
//! injected price, so every decline branch is asserted with no configured model
//! and no process-wide env mutation; the write path is driven against a tempdir.
//! Test: this file.

use super::*;

use crate::core::savings::{fold_session, savings_log_in};
use crate::core::session_model::MODEL_SOURCE_STATUSLINE;

/// A price stand-in: Sonnet's published input rate, as the shared table carries
/// it. Used so the arithmetic below is hand-checkable.
fn sonnet_price() -> Option<ParentModel> {
    Some(ParentModel {
        id: "claude-sonnet-4-6".to_string(),
        input_per_million: 3.0,
        source: MODEL_SOURCE_STATUSLINE,
    })
}

/// Why: the hand-computed delta is the whole claim. 400,000 bytes of gate output
/// are 100,000 tokens at four bytes each; the 40,000-byte compressed form is
/// 10,000; the delta is 90,000 tokens, which at Sonnet's $3/Mtok input rate is
/// $0.27.
/// Test: itself.
#[test]
fn a_hand_computed_byte_delta_matches_the_row() {
    let row = compress_row("sess-a", 400_000, 40_000, "rtk_binary", sonnet_price).expect("a row");
    assert_eq!(row.session_id, "sess-a");
    assert_eq!(row.tokens_saved, 90_000);
    assert_eq!(row.tokens_before, 100_000);
    assert!(
        (row.cost_saved_usd - 0.27).abs() < 1e-9,
        "expected $0.27, got {}",
        row.cost_saved_usd
    );
}

/// Why: the fold and any later attribution pass both key on this string, so it
/// is pinned rather than left to whatever the format call happens to emit.
/// Test: itself.
#[test]
fn a_compress_row_carries_the_named_technique() {
    let row = compress_row("sess-a", 400_000, 40_000, "rtk_binary", sonnet_price).expect("a row");
    assert_eq!(row.technique, TECHNIQUE_COMPRESS);
    assert_eq!(row.technique, "compress");
}

/// Why: `rtk_binary` and `native_fallback` compress by different mechanisms and
/// will need splitting apart later. Carrying the label in `basis` is what makes
/// that possible off the ledger line alone.
/// Test: itself.
#[test]
fn the_basis_names_the_compression_path() {
    let row =
        compress_row("sess-a", 400_000, 40_000, "native_fallback", sonnet_price).expect("a row");
    assert!(
        row.basis.contains("native_fallback"),
        "basis must name the compression path, got: {}",
        row.basis
    );
    assert!(
        row.basis.contains("claude-sonnet-4-6"),
        "basis must name the model it priced at, got: {}",
        row.basis
    );
}

/// Why: `tm compress` passes output under its size gate through unchanged. That
/// run avoided nothing, and a zero-saving row would still fold a `tokens_before`
/// into the percentage and pull the average down.
/// Test: itself.
#[test]
fn passthrough_output_writes_no_row() {
    assert!(compress_row("sess-a", 1_159, 1_159, "native_fallback", sonnet_price).is_none());
}

/// Why: compression can expand its input (a short, non-repetitive payload plus
/// a header). Rounding must never turn that into a positive claim.
/// Test: itself.
#[test]
fn an_expanded_output_writes_no_row() {
    assert!(compress_row("sess-a", 4_000, 5_000, "native_fallback", sonnet_price).is_none());
}

/// Why: a row keyed by nothing is a row the statusline can never fold, so the
/// producer declines rather than writing one.
/// Test: itself.
#[test]
fn no_row_without_a_session_id() {
    let dir = tempfile::tempdir().expect("temp dir");
    record_compress_with(
        dir.path(),
        "  ",
        "cargo test",
        400_000,
        40_000,
        "rtk_binary",
        sonnet_price,
    );
    assert!(!savings_log_in(dir.path()).exists());
}

/// Why: an unpriceable model must decline rather than substitute a guessed rate
/// — the same rule the divert producer holds.
/// Test: itself.
#[test]
fn no_row_when_the_session_model_cannot_be_priced() {
    assert!(compress_row("sess-a", 400_000, 40_000, "rtk_binary", || None).is_none());
}

/// Why: proves the whole write path — one run, one row, folding back under the
/// session it was keyed by.
/// Test: itself.
#[test]
fn a_compress_run_appends_exactly_one_row() {
    let dir = tempfile::tempdir().expect("temp dir");
    let ledger = savings_log_in(dir.path());

    record_compress_with(
        dir.path(),
        "sess-a",
        "cargo test",
        400_000,
        40_000,
        "rtk_binary",
        sonnet_price,
    );

    let contents = std::fs::read_to_string(&ledger).expect("read ledger");
    assert_eq!(
        contents.lines().filter(|l| !l.trim().is_empty()).count(),
        1,
        "one run must write exactly one row, got: {contents}"
    );
    let total = fold_session(&ledger, "sess-a");
    assert_eq!(total.rows, 1);
    assert_eq!(total.tokens_saved, 90_000);
    assert_eq!(total.tokens_before, 100_000);
}

/// Why: the compressed text is already the caller's return value by the time
/// this runs. An unwritable ledger must not turn a compression that worked into
/// one that failed.
/// Test: itself.
#[test]
fn a_ledger_write_failure_does_not_fail_the_compression() {
    let dir = tempfile::tempdir().expect("temp dir");
    // A regular file where the `usage/` directory must go: `create_dir_all`
    // inside the writer cannot succeed, so the append returns an IO error.
    let blocker = dir.path().join("usage");
    std::fs::write(&blocker, "not a directory").expect("write blocker");
    let ledger = savings_log_in(dir.path());

    record_compress_with(
        dir.path(),
        "sess-a",
        "cargo test",
        400_000,
        40_000,
        "rtk_binary",
        sonnet_price,
    );

    assert!(
        !ledger.exists(),
        "the unwritable ledger must stay unwritten, not partially created"
    );
    assert!(
        blocker.is_file(),
        "the blocking file must be left exactly as it was"
    );
}

/// #11507: a chain step writes its own tool name plus the additive chain fields,
/// and a plain run writes neither field.
/// Test: itself.
#[test]
fn a_chain_step_row_carries_its_tool_and_the_chain_fields() {
    let dir = tempfile::tempdir().expect("temp dir");
    let chain = ChainTag {
        shape: "cd ; git diff && cargo test",
        steps: 3,
    };
    record_tagged_with(
        dir.path(),
        "sess-c",
        ("cargo test", Some(chain)),
        (400_000, 40_000),
        "native_fallback",
        sonnet_price,
    );
    record_compress_with(
        dir.path(),
        "sess-c",
        "git diff",
        400_000,
        40_000,
        "native_fallback",
        sonnet_price,
    );
    let text = std::fs::read_to_string(savings_log_in(dir.path())).expect("ledger");
    let rows: Vec<serde_json::Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).expect("json"))
        .collect();
    assert_eq!(rows.len(), 2, "{text}");
    assert_eq!(rows[0]["tool_name"], "cargo test");
    assert_eq!(rows[0]["chain_shape"], "cd ; git diff && cargo test");
    assert_eq!(rows[0]["steps"], 3);
    assert!(rows[1].get("chain_shape").is_none(), "{text}");
    assert!(rows[1].get("steps").is_none(), "{text}");
}

/// Run `f` under a subscriber that keeps INFO and above; return what it logged.
///
/// #11560: `tm compress` installs an `info` filter, so a line the test sees
/// here is a line the operator sees there.
pub(crate) fn logs_at_info(f: impl FnOnce()) -> String {
    use std::sync::{Arc, Mutex};
    let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
    let sink = Arc::clone(&buf);
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_ansi(false)
        .with_writer(move || SharedBuf(Arc::clone(&sink)))
        .finish();
    tracing::subscriber::with_default(subscriber, f);
    let bytes = buf.lock().map(|b| b.clone()).unwrap_or_default();
    String::from_utf8_lossy(&bytes).into_owned()
}

struct SharedBuf(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for SharedBuf {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        if let Ok(mut b) = self.0.lock() {
            b.extend_from_slice(data);
        }
        Ok(data.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Why (#11560): a zero-saving decline was invisible; it must be counted in a
/// file, and #7607 forbids it on `tm compress` stderr (the agent's tool result).
/// Test: itself.
#[test]
fn a_zero_saving_decline_is_recorded_in_the_decline_file() {
    let dir = tempfile::tempdir().expect("temp dir");
    let run = || {
        record_compress_with(
            dir.path(),
            "sess-a",
            "ls",
            668,
            668,
            "native_fallback",
            sonnet_price,
        )
    };
    let logged = logs_at_info(run);
    assert!(
        logged.is_empty(),
        "#7607: nothing on stderr at info: {logged}"
    );
    let day = format!("{}.jsonl", &now_ts()[..10]);
    let text = std::fs::read_to_string(decline_dir_in(dir.path()).join(day)).expect("day file");
    assert_eq!(text.lines().count(), 1, "{text}");
    let line: serde_json::Value = serde_json::from_str(text.trim()).expect("json");
    assert_eq!(line["reason"], "no_gain");
    assert_eq!(line["session_id"], "sess-a");
    assert!(!savings_log_in(dir.path()).exists(), "still no row");
}

/// #11560: a failed decline write is swallowed with nothing at info.
#[test]
fn a_failed_decline_write_is_silent() {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(dir.path().join("usage"), "not a directory").expect("blocker");
    let logged = logs_at_info(|| {
        record_compress_with(dir.path(), "sess-a", "ls", 668, 668, "p", sonnet_price);
    });
    assert!(logged.is_empty(), "{logged}");
}

/// Why (#11560): a failed savings write is fail-open AND visible.
/// Test: itself.
#[test]
fn a_failed_savings_write_is_logged() {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(dir.path().join("usage"), "not a directory").expect("blocker");
    let logged = logs_at_info(|| {
        record_compress_with(
            dir.path(),
            "sess-a",
            "cargo test",
            400_000,
            40_000,
            "rtk_binary",
            sonnet_price,
        );
    });
    assert!(
        logged.contains("could not append the compress savings row"),
        "{logged}"
    );
}

/// #11560: the observer's split and the row decline agree at the boundary.
#[test]
fn compress_gains_matches_the_row_decline() {
    for (before, after) in [(668, 668), (400_000, 40_000), (8, 5), (9, 4), (0, 0)] {
        assert_eq!(
            compress_gains(before, after),
            compress_row("s", before, after, "p", sonnet_price).is_some(),
            "{before} -> {after}"
        );
    }
}

/// #11560 review: the decline sink is day-capped; a full day drops silently.
#[test]
fn a_full_decline_day_drops_silently() {
    let dir = tempfile::tempdir().expect("temp dir");
    let declines = decline_dir_in(dir.path());
    let line = serde_json::json!({"ts": "2026-11-22T00:00:00Z", "reason": "no_gain"});
    let first = append_day_line(&declines, "2026-11-22T00:00:00Z", &line, 10, 14).expect("write");
    assert_eq!(
        first,
        crate::core::savings_opportunity::OpportunityAppend::Written
    );
    let day = declines.join("2026-11-22.jsonl");
    let len = std::fs::metadata(&day).expect("day file").len();
    let logged = logs_at_info(|| {
        let again = append_day_line(&declines, "2026-11-22T00:00:01Z", &line, 10, 14);
        assert!(matches!(
            again,
            Ok(crate::core::savings_opportunity::OpportunityAppend::DayFullMarked)
        ));
    });
    assert!(logged.is_empty(), "{logged}");
    let after = std::fs::read_to_string(&day).expect("day file");
    assert!(
        after.len() as u64 > len && after.contains("day_full"),
        "{after}"
    );
    assert_eq!(
        after.matches("no_gain").count(),
        1,
        "the dropped line is not stored"
    );
}

/// #11563: `ls <path>` and `grep -n <pattern>` put the path or the pattern in
/// `tool_name`; both ledgers (savings and opportunity) must store neither.
#[test]
fn a_compress_row_never_stores_a_path_or_pattern() {
    let dir = tempfile::tempdir().expect("temp dir");
    for tool in ["ls /Users/x/p", "grep -n secret f"] {
        record_compress_with(
            dir.path(),
            "sess-p",
            tool,
            400_000,
            40_000,
            "native_fallback",
            sonnet_price,
        );
    }
    let opportunity = [
        crate::core::savings_opportunity::OpportunityRow::new(
            "PostToolUse",
            "s",
            "bash",
            "ls /Users/x/p",
            9,
            9,
            1,
        ),
        crate::core::savings_opportunity::OpportunityRow::new(
            "PostToolUse",
            "s",
            "bash",
            "grep -n secret f",
            9,
            9,
            1,
        ),
    ];
    let text = std::fs::read_to_string(savings_log_in(dir.path())).expect("ledger");
    let mut names: Vec<String> = text
        .lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).expect("json"))
        .map(|v| v["tool_name"].as_str().unwrap_or_default().to_string())
        .collect();
    names.extend(opportunity.iter().map(|r| r.tool_name.clone()));
    assert_eq!(names, ["ls", "grep", "ls", "grep"], "{text}");
    for name in &names {
        assert!(!name.contains('/') && !name.contains("secret"), "{name}");
    }
}
