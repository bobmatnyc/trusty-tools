//! Tests for the one-shot savings-ledger repair (#7569).
//!
//! Why: this is the only code in the tree that REWRITES the operator's ledger,
//! and the ledger is the sole copy of every genuine row on it. So the fixture
//! below is built from the exact shapes measured on the real file — the
//! 13-byte fixture basis keyed by a directory name, the same basis keyed by a
//! real session UUID, a genuine fold, the one `compiled 10000 B` oddity that
//! must survive, and a #7579 line holding two rows with no separator — and the
//! assertions pin the kept and quarantined sets exactly rather than counting.
//! What: a temp-directory ledger written line by line, so a malformed line is
//! literally malformed bytes rather than a struct that could not exist.
//! Test: this file.

use super::*;
use crate::core::savings::{SavingsRow, savings_log_in};

/// A genuine `compress` row — nothing about it matches the predicate.
const KEEP_COMPRESS: &str = r#"{"ts":"2026-09-10T03:41:19Z","session_id":"11111111-1111-4111-8111-111111111111","technique":"compress","tokens_saved":260,"tokens_before":269,"cost_saved_usd":0.0026,"basis":"output 269 tok - compressed 9 tok, at 4 B/token, via rtk_binary, priced at claude-fable-5-1 (statusline) input $10/Mtok","model_source":"statusline"}"#;

/// The #7514 fixture row keyed by the fixture DIRECTORY name.
const DROP_FIXTURE_B: &str = r#"{"ts":"2026-09-10T04:00:00Z","session_id":"b","technique":"instruction-compression","tokens_saved":5636,"tokens_before":5639,"cost_saved_usd":0.0169,"basis":"sources 22559 B - compiled 13 B, at 4 B/token, priced at claude-sonnet-4-5 input $3/Mtok","model_source":"config-fallback"}"#;

/// The same fixture row keyed by the REAL session UUID the test process had
/// exported — half the operator's polluted rows look like this.
const DROP_FIXTURE_UUID: &str = r#"{"ts":"2026-09-10T04:00:01Z","session_id":"22222222-2222-4222-8222-222222222222","technique":"instruction-compression","tokens_saved":6699,"tokens_before":6702,"cost_saved_usd":0.0201,"basis":"sources 26810 B - compiled 13 B, at 4 B/token, priced at claude-sonnet-4-5 input $3/Mtok","model_source":"statusline"}"#;

/// A genuine instruction-compression row — a real compiled prompt.
const KEEP_REAL_FOLD: &str = r#"{"ts":"2026-09-11T09:12:00Z","session_id":"33333333-3333-4333-8333-333333333333","technique":"instruction-compression","tokens_saved":611,"tokens_before":6702,"cost_saved_usd":0.0018,"basis":"sources 26810 B - compiled 24363 B, at 4 B/token, priced at claude-sonnet-4-5 input $3/Mtok","model_source":"statusline"}"#;

/// The one `compiled 10000 B` row on the operator's ledger. It shares the
/// fixture's SOURCE byte count, which is why the predicate must not key on
/// that — this row is kept.
const KEEP_ODDITY: &str = r#"{"ts":"2026-09-09T21:40:00Z","session_id":"44444444-4444-4444-8444-444444444444","technique":"instruction-compression","tokens_saved":3139,"tokens_before":5639,"cost_saved_usd":0.0094,"basis":"sources 22559 B - compiled 10000 B, at 4 B/token, priced at claude-sonnet-4-5 input $3/Mtok","model_source":"statusline"}"#;

/// A genuine `divert` row, used as the first half of the #7579 line.
const KEEP_DIVERT: &str = r#"{"ts":"2026-09-11T10:00:00Z","session_id":"55555555-5555-4555-8555-555555555555","technique":"divert","tokens_saved":5300,"tokens_before":5500,"cost_saved_usd":0.0159,"basis":"files 5000 tok - summary 200 tok, at 4 B/token, priced at claude-fable-5-1 (statusline) input $10/Mtok, less worker $0.0001","model_source":"statusline"}"#;

/// A row cut off mid-object — what a single interrupted write leaves.
const TRUNCATED: &str = r#"{"ts":"2026-09-11T10:00:01Z","session_id":"6666"#;

/// The ledger the classification tests share, in physical-line order.
///
/// Line 6 is the #7579 shape: two complete rows, no separator. Line 7 is the
/// blank line the two stranded newlines leave behind. Line 8 is a complete row
/// followed by a truncated one.
fn fixture_lines() -> Vec<String> {
    vec![
        KEEP_COMPRESS.to_string(),
        DROP_FIXTURE_B.to_string(),
        DROP_FIXTURE_UUID.to_string(),
        KEEP_REAL_FOLD.to_string(),
        KEEP_ODDITY.to_string(),
        format!("{KEEP_DIVERT}{DROP_FIXTURE_B}"),
        String::new(),
        format!("{KEEP_COMPRESS}{TRUNCATED}"),
    ]
}

/// Write the fixture ledger under a fresh temp root and return both.
fn fixture_ledger() -> (tempfile::TempDir, std::path::PathBuf) {
    let root = tempfile::tempdir().expect("temp root");
    let ledger = savings_log_in(root.path());
    std::fs::create_dir_all(ledger.parent().expect("parent")).expect("mkdir");
    std::fs::write(&ledger, format!("{}\n", fixture_lines().join("\n"))).expect("write");
    (root, ledger)
}

/// A fixed instant, so a sidecar name is a value the assertions can spell.
fn at(rfc3339: &str) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339(rfc3339)
        .expect("fixture timestamp")
        .with_timezone(&chrono::Utc)
}

fn parse(line: &str) -> SavingsRow {
    serde_json::from_str(line).expect("fixture row parses")
}

/// Why (#7569): this is THE rule the repair enforces, and every candidate
/// discriminator measured on the operator's ledger except this one has a false
/// positive on a genuine row — `sources 22559 B` matches [`KEEP_ODDITY`], and
/// `session_id` matches only half the polluted rows. A predicate that keyed on
/// either would delete real savings or miss 115 fixture rows.
/// Test: itself.
#[test]
fn the_predicate_matches_only_fixture_rows() {
    assert!(is_test_fixture_row(&parse(DROP_FIXTURE_B)));
    assert!(is_test_fixture_row(&parse(DROP_FIXTURE_UUID)));
    assert!(
        !is_test_fixture_row(&parse(KEEP_ODDITY)),
        "the 10000 B row shares the fixture's source count and must survive"
    );
    assert!(!is_test_fixture_row(&parse(KEEP_REAL_FOLD)));
    assert!(!is_test_fixture_row(&parse(KEEP_COMPRESS)));
    assert!(!is_test_fixture_row(&parse(KEEP_DIVERT)));

    let mut near_miss = parse(DROP_FIXTURE_B);
    near_miss.basis = "sources 22559 B - compiled 130 B".to_string();
    assert!(
        !is_test_fixture_row(&near_miss),
        "the trailing ' B' must stop `compiled 13 B` matching `compiled 130 B`"
    );

    let mut other_technique = parse(DROP_FIXTURE_B);
    other_technique.technique = "compress".to_string();
    assert!(
        !is_test_fixture_row(&other_technique),
        "the basis alone is not the rule; the technique is half of it"
    );
}

/// Why: the dry run prints this verdict and the apply enforces it, so the exact
/// kept and quarantined sets — not just their sizes — are what the command's
/// safety claim rests on.
/// Test: itself.
#[test]
fn plan_classifies_a_mixed_ledger() {
    let (_root, ledger) = fixture_ledger();
    let planned = plan(&ledger).expect("plan");

    let kept: Vec<&str> = planned.kept().map(|f| f.text.as_str()).collect();
    assert_eq!(
        kept,
        vec![
            KEEP_COMPRESS,
            KEEP_REAL_FOLD,
            KEEP_ODDITY,
            KEEP_DIVERT,
            KEEP_COMPRESS,
        ],
        "every genuine row survives, in ledger order"
    );

    let quarantined: Vec<(&str, &'static str)> = planned
        .quarantined()
        .map(|f| {
            (
                f.text.as_str(),
                f.quarantine
                    .expect("a quarantined fragment has a reason")
                    .label(),
            )
        })
        .collect();
    assert_eq!(
        quarantined,
        vec![
            (DROP_FIXTURE_B, "test-fixture"),
            (DROP_FIXTURE_UUID, "test-fixture"),
            (DROP_FIXTURE_B, "test-fixture"),
            (TRUNCATED, "malformed"),
        ]
    );

    assert_eq!(planned.count_of(Reason::TestFixture), 3);
    assert_eq!(planned.count_of(Reason::Malformed), 1);
    assert_eq!(planned.lines_read, 8);
    assert_eq!(planned.blank_lines, vec![7]);
    assert!(!planned.is_clean());
}

/// Why (#7579): a physical line holding two complete rows must yield two
/// fragments judged independently — the line the operator's ledger carries has
/// a genuine row and a fixture row on it, and keeping or dropping both together
/// would be wrong either way.
/// Test: itself.
#[test]
fn plan_splits_a_concatenated_line() {
    let (_root, ledger) = fixture_ledger();
    let planned = plan(&ledger).expect("plan");

    let line_six: Vec<&Fragment> = planned.fragments.iter().filter(|f| f.line == 6).collect();
    assert_eq!(line_six.len(), 2, "one physical line, two rows");
    assert_eq!(line_six[0].text, KEEP_DIVERT);
    assert_eq!(line_six[0].quarantine, None);
    assert_eq!(line_six[1].text, DROP_FIXTURE_B);
    assert_eq!(line_six[1].quarantine, Some(Reason::TestFixture));

    assert_eq!(
        planned.repaired_lines,
        vec![6, 8],
        "only the two structurally broken lines are reported"
    );
}

/// Why: a line that is not JSON at all must be reported once, not split into a
/// cascade of fragments — a repair that multiplies its own findings is one an
/// operator cannot read.
/// Test: itself.
#[test]
fn split_line_reports_an_unparseable_line_once() {
    let root = tempfile::tempdir().expect("temp root");
    let ledger = savings_log_in(root.path());
    std::fs::create_dir_all(ledger.parent().expect("parent")).expect("mkdir");
    std::fs::write(&ledger, "not json at all\n").expect("write");

    let planned = plan(&ledger).expect("plan");
    assert_eq!(planned.fragments.len(), 1);
    assert_eq!(planned.fragments[0].text, "not json at all");
    assert_eq!(planned.fragments[0].quarantine, Some(Reason::Malformed));
}

/// Why: a machine that has run no producer has no ledger, which is the ordinary
/// state and not a fault — the command must report "nothing to do", not fail.
/// Test: itself.
#[test]
fn plan_of_a_missing_ledger_is_clean() {
    let root = tempfile::tempdir().expect("temp root");
    let planned = plan(&savings_log_in(root.path())).expect("plan");
    assert!(planned.is_clean());
    assert_eq!(planned.lines_read, 0);
}

/// Why: an unreadable ledger must never be mistaken for an empty one — that
/// mistake would have `--apply` rewrite the operator's ledger to nothing.
/// Test: itself.
#[test]
fn plan_propagates_a_read_error() {
    let root = tempfile::tempdir().expect("temp root");
    let not_a_file = root.path().join("usage");
    std::fs::create_dir_all(&not_a_file).expect("mkdir");
    assert!(
        plan(&not_a_file).is_err(),
        "a directory where a ledger should be is an error, not an empty plan"
    );
}

/// Why: the sidecar has to land beside the ledger so the rewrite's rename stays
/// on one filesystem, and has to carry the run's instant so two repairs cannot
/// collide.
/// Test: itself.
#[test]
fn quarantine_path_is_a_timestamped_sibling() {
    let path = quarantine_path(
        std::path::Path::new("/x/usage/savings.jsonl"),
        at("2026-09-12T01:02:03Z"),
    );
    assert_eq!(
        path,
        std::path::PathBuf::from("/x/usage/savings.jsonl.quarantine-20260912T010203Z.jsonl")
    );
}

/// Why: the whole point of the command. The ledger must come back holding only
/// genuine rows, and every removed row must be recoverable from the sidecar
/// with the rule that took it.
/// Test: itself.
#[test]
fn apply_quarantines_and_rewrites() {
    let (_root, ledger) = fixture_ledger();
    let planned = plan(&ledger).expect("plan");
    let applied = apply(&ledger, &planned, at("2026-09-12T01:02:03Z")).expect("apply");

    assert_eq!(applied.kept, 5);
    assert_eq!(applied.quarantined, 4);

    let rewritten = std::fs::read_to_string(&ledger).expect("read ledger");
    assert_eq!(
        rewritten,
        format!(
            "{KEEP_COMPRESS}\n{KEEP_REAL_FOLD}\n{KEEP_ODDITY}\n{KEEP_DIVERT}\n{KEEP_COMPRESS}\n"
        ),
        "one row per line, in order, with no blank lines left"
    );

    let sidecar = std::fs::read_to_string(&applied.quarantine).expect("read sidecar");
    let entries: Vec<QuarantinedRow> = sidecar
        .lines()
        .map(|line| serde_json::from_str(line).expect("sidecar entry parses"))
        .collect();
    assert_eq!(entries.len(), 4);
    assert_eq!(entries[0].reason, "test-fixture");
    assert_eq!(entries[0].source_line, 2);
    assert_eq!(entries[0].raw, DROP_FIXTURE_B);
    assert_eq!(entries[3].reason, "malformed");
    assert_eq!(entries[3].source_line, 8);
    assert_eq!(entries[3].raw, TRUNCATED);
}

/// Why: the three ledger readers must fold the survivors exactly as they folded
/// them before, which they only do if the repair writes back the original bytes
/// rather than a re-serialisation.
/// Test: itself.
#[test]
fn apply_keeps_the_original_bytes() {
    let (_root, ledger) = fixture_ledger();
    let before = crate::core::savings::fold_all(&ledger);
    let planned = plan(&ledger).expect("plan");
    apply(&ledger, &planned, at("2026-09-12T01:02:03Z")).expect("apply");

    let survivors: Vec<String> = std::fs::read_to_string(&ledger)
        .expect("read")
        .lines()
        .map(str::to_string)
        .collect();
    for line in &survivors {
        assert!(
            fixture_lines().iter().any(|original| original == line)
                || line == KEEP_DIVERT
                || line == KEEP_COMPRESS,
            "a kept row must be a byte-identical fragment of the original: {line}"
        );
    }

    let after = crate::core::savings::fold_all(&ledger);
    assert!(
        after.tokens_saved < before.tokens_saved,
        "the fixture rows' tokens must be gone from the fold"
    );
    assert_eq!(
        after.rows, 5,
        "every genuine row still folds, including the two rescued from broken lines"
    );
}

/// Why: the issue's closure condition is that a repair run leaves the polluted
/// count at zero, which only means anything if re-running is a no-op rather
/// than a second rewrite and a second sidecar.
/// Test: itself.
#[test]
fn a_second_apply_finds_nothing() {
    let (_root, ledger) = fixture_ledger();
    let first = plan(&ledger).expect("plan");
    apply(&ledger, &first, at("2026-09-12T01:02:03Z")).expect("apply");
    let after_first = std::fs::read_to_string(&ledger).expect("read");

    let second = plan(&ledger).expect("re-plan");
    assert!(second.is_clean(), "a repaired ledger plans to nothing");
    let applied = apply(&ledger, &second, at("2026-09-12T02:00:00Z")).expect("second apply");
    assert_eq!(applied.quarantined, 0);
    assert!(
        !applied.quarantine.exists(),
        "a clean plan writes no sidecar"
    );
    assert_eq!(
        std::fs::read_to_string(&ledger).expect("read"),
        after_first,
        "the ledger is byte-identical after the second run"
    );
}

/// Why: the rename is the only irreversible step, so every failure before it
/// must leave the ledger exactly as it was. A sidecar that already exists is
/// the reachable case — two runs inside one second.
/// Test: itself.
#[test]
fn apply_leaves_the_ledger_untouched_when_the_sidecar_exists() {
    let (_root, ledger) = fixture_ledger();
    let original = std::fs::read_to_string(&ledger).expect("read");
    let now = at("2026-09-12T01:02:03Z");
    std::fs::write(quarantine_path(&ledger, now), "occupied").expect("pre-existing sidecar");

    let planned = plan(&ledger).expect("plan");
    let failure = apply(&ledger, &planned, now).expect_err("must refuse to clobber the sidecar");
    assert_eq!(failure.kind(), std::io::ErrorKind::AlreadyExists);
    assert_eq!(
        std::fs::read_to_string(&ledger).expect("read"),
        original,
        "the ledger must survive a failed repair unchanged"
    );
}

/// Why: the marker directory the repair moves has to be the one the producer
/// writes, or the sweep moves nothing and reports success.
/// Test: itself.
#[test]
fn marker_dir_nests_under_usage() {
    assert_eq!(
        marker_dir(std::path::Path::new("/root")),
        std::path::PathBuf::from("/root/usage/no-fold-warned")
    );
}

/// Why: the marker is a warn-once cache, so moving the directory aside is safe
/// by construction — but only if every file goes with it and none is deleted.
/// Test: itself.
#[test]
fn quarantine_markers_moves_the_whole_directory() {
    let root = tempfile::tempdir().expect("temp root");
    let dir = marker_dir(root.path());
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(dir.join("0002e2d5001ed230"), "22559 24667").expect("marker");
    std::fs::write(dir.join("000a4d58537117bb"), "26810 29172").expect("marker");
    assert_eq!(count_markers(root.path()).expect("count"), 2);

    let (moved, markers) = quarantine_markers(root.path(), at("2026-09-12T01:02:03Z"))
        .expect("quarantine")
        .expect("a directory was there");
    assert_eq!(markers, 2);
    assert_eq!(
        moved.file_name().expect("name"),
        std::ffi::OsStr::new("no-fold-warned.quarantine-20260912T010203Z")
    );
    assert!(!dir.exists(), "the live directory is moved, not copied");
    assert_eq!(
        std::fs::read_to_string(moved.join("000a4d58537117bb")).expect("read"),
        "26810 29172",
        "every marker survives the move"
    );
    assert_eq!(count_markers(root.path()).expect("count"), 0);
}

/// Why: a machine whose producer never declined has no marker directory, and
/// `--markers` there must report nothing rather than fail.
/// Test: itself.
#[test]
fn quarantine_markers_of_a_missing_directory_is_none() {
    let root = tempfile::tempdir().expect("temp root");
    assert_eq!(count_markers(root.path()).expect("count"), 0);
    assert!(
        quarantine_markers(root.path(), at("2026-09-12T01:02:03Z"))
            .expect("quarantine")
            .is_none()
    );
}
