//! Unit tests for cost computation and the `PerfCollector` lifecycle.
//!
//! Why: Cost math underpins every billing/UX figure; the collector's
//! record/flush behavior is the persistence contract.
//! What: `cost_usd_*` pricing cases, `truncate_preview`/`filename_stamp`
//! formatters, and collector record/total/flush tests.
//! Test: This module is itself the test coverage.

use super::*;

#[test]
fn token_usage_default_is_zeros() {
    let u = TokenUsage::default();
    assert_eq!(u.prompt_tokens, 0);
    assert_eq!(u.completion_tokens, 0);
    assert_eq!(u.cache_read_tokens, 0);
    assert_eq!(u.cache_creation_tokens, 0);
}

#[test]
fn token_usage_accumulates() {
    let mut a = TokenUsage::new(10, 5, 2, 1);
    a.add(&TokenUsage::new(3, 7, 0, 4));
    assert_eq!(a, TokenUsage::new(13, 12, 2, 5));
}

#[test]
fn cost_usd_known_sonnet() {
    // 1M prompt tokens of sonnet = $3.00
    let c = cost_usd("anthropic/claude-sonnet-4-5", 1_000_000, 0, 0, 0);
    assert!((c - 3.0).abs() < 1e-9, "got {c}");
}

#[test]
fn cost_usd_known_haiku() {
    // 1M output tokens of haiku = $4.00
    let c = cost_usd("anthropic/claude-haiku-4", 0, 1_000_000, 0, 0);
    assert!((c - 4.0).abs() < 1e-9, "got {c}");
}

#[test]
fn cost_usd_cache_read_is_cheaper() {
    // Cache-read is 10x cheaper than fresh input on Sonnet.
    let fresh = cost_usd("claude-sonnet-4-6", 1_000_000, 0, 0, 0);
    let cached = cost_usd("claude-sonnet-4-6", 0, 0, 1_000_000, 0);
    assert!(cached < fresh);
    assert!((cached - 0.30).abs() < 1e-9);
}

// --- #29: Cost calculation tests for cache hits ---

#[test]
fn cost_usd_sonnet_cache_creation_matches_spec() {
    // #29: Cache write rate for claude-sonnet-4-6 is $3.75/MTok.
    // 1M cache_creation tokens should cost exactly $3.75.
    let c = cost_usd("anthropic/claude-sonnet-4-6", 0, 0, 0, 1_000_000);
    assert!(
        (c - 3.75).abs() < 1e-9,
        "expected $3.75 for 1M cache write tokens on sonnet-4-6, got {c}"
    );
}

#[test]
fn cost_usd_sonnet_cache_read_is_one_tenth_of_input() {
    // #29: Cache read is $0.30/MTok vs input $3.00/MTok — exactly 10%.
    // This is the headline savings figure for prompt caching.
    let fresh_input = cost_usd("claude-sonnet-4-6", 1_000_000, 0, 0, 0);
    let cache_read = cost_usd("claude-sonnet-4-6", 0, 0, 1_000_000, 0);
    let ratio = cache_read / fresh_input;
    assert!(
        (ratio - 0.10).abs() < 1e-9,
        "cache_read should be 10% of input cost, got {ratio} (fresh={fresh_input}, cached={cache_read})"
    );
}

#[test]
fn cost_usd_sonnet_mixed_cache_hit_scenario() {
    // #29: Realistic scenario — a turn where most input is a cache hit:
    // 100 fresh prompt tokens + 50 completion + 9000 cache_read + 1000 cache_creation.
    // Sonnet rates: $3/MTok in, $15/MTok out, $0.30/MTok cache_r, $3.75/MTok cache_w.
    let c = cost_usd("anthropic/claude-sonnet-4-6", 100, 50, 9_000, 1_000);
    // 100 * 3e-6 = 0.0003, 50 * 15e-6 = 0.00075, 9000 * 0.30e-6 = 0.0027,
    // 1000 * 3.75e-6 = 0.00375. Total = 0.00750.
    let expected = 0.0003 + 0.00075 + 0.0027 + 0.00375;
    assert!((c - expected).abs() < 1e-9, "expected {expected}, got {c}");
}

#[test]
fn cost_usd_unknown_defaults_to_sonnet() {
    let u = cost_usd("some/unknown-model", 1_000_000, 0, 0, 0);
    assert!((u - 3.0).abs() < 1e-9);
}

#[test]
fn collector_records_phases() {
    let mut c = PerfCollector::new(7, "prescriptive", "write x");
    c.record_phase(
        "research",
        500,
        "claude-sonnet-4-5",
        &TokenUsage::new(1000, 500, 0, 0),
    );
    c.record_phase(
        "code",
        1200,
        "claude-sonnet-4-5",
        &TokenUsage::new(2000, 1000, 500, 200),
    );
    let r = c.build_record();
    assert_eq!(r.phases.len(), 2);
    assert_eq!(r.phases[0].prompt_tokens, 1000);
    assert_eq!(r.totals.prompt_tokens, 3000);
    assert_eq!(r.totals.completion_tokens, 1500);
    assert_eq!(r.totals.cache_read_tokens, 500);
    assert_eq!(r.totals.cache_creation_tokens, 200);
    // Sanity: some cost accrued.
    assert!(r.totals.cost_usd > 0.0);
    assert_eq!(r.build, 7);
    assert_eq!(r.workflow, "prescriptive");
}

#[test]
fn truncate_preview_respects_limit() {
    let s = "x".repeat(200);
    let p = truncate_preview(&s, 120);
    assert_eq!(p.chars().count(), 123, "120 chars + ellipsis");
    assert!(p.ends_with("..."));
}

#[test]
fn truncate_preview_short_string_unchanged() {
    let p = truncate_preview("hi", 120);
    assert_eq!(p, "hi");
}

#[test]
fn filename_stamp_format() {
    let s = filename_stamp("2026-04-22T17:31:30Z", 42);
    assert_eq!(s, "20260422-173130-build42");
}

#[tokio::test]
async fn collector_flush_records_failed_status() {
    // #56: When a workflow phase fails, the engine sets status=partial
    // and records the failing phase, then flushes. Verify the JSON round-
    // trips those fields.
    let tmp = tempfile::tempdir().unwrap();
    let mut c = PerfCollector::new(11, "test-wf", "broken task");
    c.record_phase(
        "research",
        50,
        "claude-sonnet-4-6",
        &TokenUsage::new(100, 50, 0, 0),
    );
    c.set_status("partial");
    c.set_failed_phase("plan");
    c.flush(tmp.path()).await.unwrap();

    let runs = tmp.path().join("runs");
    let mut entries = tokio::fs::read_dir(&runs).await.unwrap();
    let mut rec: Option<PerfRecord> = None;
    while let Some(e) = entries.next_entry().await.unwrap() {
        if e.path().extension().and_then(|s| s.to_str()) == Some("json") {
            let bytes = tokio::fs::read(e.path()).await.unwrap();
            rec = Some(serde_json::from_slice(&bytes).unwrap());
        }
    }
    let rec = rec.expect("perf json written");
    assert_eq!(rec.status, "partial");
    assert_eq!(rec.failed_phase.as_deref(), Some("plan"));
}

#[test]
fn collector_default_status_is_success() {
    // #56: New collectors default to status=success so clean runs don't
    // need to explicitly call set_status.
    let c = PerfCollector::new(1, "wf", "task");
    let r = c.build_record();
    assert_eq!(r.status, "success");
    assert!(r.failed_phase.is_none());
}

/// Why: Fix 3 / claude-mpm parity — when the QA agent emits structured
/// pass/fail counts, those counts must round-trip through the perf
/// record's JSON form AND appear in the `runs.log` summary line.
/// What: Constructs a `PerfCollector`, sets test counts, flushes, and
/// asserts both the JSON file and the log line carry "42" and "0".
/// Test: this function (`test_run_record_serializes_test_counts`).
#[tokio::test]
async fn test_run_record_serializes_test_counts() {
    let tmp = tempfile::tempdir().unwrap();
    let mut c = PerfCollector::new(99, "test-wf", "task with tests");
    c.record_phase("qa", 10, "claude-sonnet-4-6", &TokenUsage::new(10, 5, 0, 0));
    c.set_test_counts(42, 0);
    c.flush(tmp.path()).await.unwrap();

    // JSON round-trip retains the counts.
    let runs = tmp.path().join("runs");
    let mut entries = tokio::fs::read_dir(&runs).await.unwrap();
    let mut rec: Option<PerfRecord> = None;
    while let Some(e) = entries.next_entry().await.unwrap() {
        if e.path().extension().and_then(|s| s.to_str()) == Some("json") {
            let bytes = tokio::fs::read(e.path()).await.unwrap();
            rec = Some(serde_json::from_slice(&bytes).unwrap());
        }
    }
    let rec = rec.expect("perf json written");
    assert_eq!(rec.tests_passed, Some(42));
    assert_eq!(rec.tests_failed, Some(0));

    // Raw JSON bytes contain the literal "42" and "0" values.
    let json_str = serde_json::to_string(&rec).unwrap();
    assert!(json_str.contains("\"tests_passed\":42"));
    assert!(json_str.contains("\"tests_failed\":0"));

    // runs.log line ends with the new columns.
    let log = tokio::fs::read_to_string(tmp.path().join("runs.log"))
        .await
        .unwrap();
    assert!(
        log.contains("tests_passed=42"),
        "runs.log missing tests_passed=42: {log}"
    );
    assert!(
        log.contains("tests_failed=0"),
        "runs.log missing tests_failed=0: {log}"
    );
}

/// Why: Back-compat — when QA didn't run or emitted no JSON envelope,
/// the counts must be `None`, omitted from JSON via `skip_serializing_if`,
/// and rendered as `-` in `runs.log`.
#[tokio::test]
async fn run_record_omits_test_counts_when_none() {
    let tmp = tempfile::tempdir().unwrap();
    let mut c = PerfCollector::new(100, "test-wf", "no qa");
    c.record_phase(
        "research",
        5,
        "claude-sonnet-4-6",
        &TokenUsage::new(10, 5, 0, 0),
    );
    c.flush(tmp.path()).await.unwrap();

    let log = tokio::fs::read_to_string(tmp.path().join("runs.log"))
        .await
        .unwrap();
    assert!(
        log.contains("tests_passed=-"),
        "expected '-' placeholder: {log}"
    );
    assert!(
        log.contains("tests_failed=-"),
        "expected '-' placeholder: {log}"
    );
}

#[tokio::test]
async fn collector_flush_writes_json_and_log() {
    let tmp = tempfile::tempdir().unwrap();
    let mut c = PerfCollector::new(5, "test-wf", "hello task");
    c.record_phase(
        "plan",
        100,
        "claude-sonnet-4-5",
        &TokenUsage::new(100, 50, 0, 0),
    );
    c.flush(tmp.path()).await.unwrap();

    // A runs/ dir was created with at least one .json inside.
    let runs = tmp.path().join("runs");
    assert!(runs.exists());
    let mut entries = tokio::fs::read_dir(&runs).await.unwrap();
    let mut found_json = false;
    while let Some(e) = entries.next_entry().await.unwrap() {
        if e.path().extension().and_then(|s| s.to_str()) == Some("json") {
            found_json = true;
            let bytes = tokio::fs::read(e.path()).await.unwrap();
            let rec: PerfRecord = serde_json::from_slice::<PerfRecord>(&bytes).unwrap();
            assert_eq!(rec.build, 5);
            assert_eq!(rec.workflow, "test-wf");
            assert_eq!(rec.phases.len(), 1);
            assert_eq!(rec.phases[0].name, "plan");
        }
    }
    assert!(found_json, "expected at least one .json run file");

    // runs.log exists and contains the build tag.
    let log = tokio::fs::read_to_string(tmp.path().join("runs.log"))
        .await
        .unwrap();
    assert!(log.contains("build=5"));
    assert!(log.contains("workflow=test-wf"));
}
