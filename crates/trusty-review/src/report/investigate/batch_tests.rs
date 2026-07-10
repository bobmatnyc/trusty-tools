//! Tests for multi-batch investigation orchestration (#2357, wave 3.1).
//!
//! Why: this is the regression test for the live-QA incident — a single request
//! sending too much content hit the output-token ceiling and discarded ALL
//! findings.  These tests prove the structural fix: partitioning math, the
//! retry-once-then-fail-closed state machine, that one failed batch never takes
//! down its siblings' findings, and that merged results still dedupe correctly.
//! What: partition sizing/ordering/edge cases; a scripted mock provider drives
//! the truncate→retry→recover path, the truncate→retry→still-truncated path
//! (batch fails, others survive), and the all-batches-fail path; a standalone
//! unit test on `merge_dedupe`'s severity-upgrade behaviour.
//! Test: included as `#[cfg(test)] mod tests` from `batch.rs`.

use std::collections::VecDeque;
use std::sync::Mutex;

use async_trait::async_trait;

use crate::llm::{LlmError, LlmProvider, LlmRequest, LlmResponse};
use crate::report::investigate::select::{SelectedFile, Selection};
use crate::report::investigate::verify::VerifiedFinding;
use crate::report::metrics::Severity;

use super::*;

fn selfile(path: &str, content: &str) -> SelectedFile {
    SelectedFile {
        path: path.to_string(),
        content: content.to_string(),
        truncated: false,
        dimensions: vec![],
    }
}

// ── Partitioning ────────────────────────────────────────────────────────────

/// Why: this is the structural bound — batches must actually respect the cap.
/// What: three 40 KiB files split 2-then-1 across the 90 KiB cap, in order.
/// Test: this test itself.
#[test]
fn partitions_by_size() {
    let files = vec![
        selfile("a.rs", &"x".repeat(40 * 1024)),
        selfile("b.rs", &"x".repeat(40 * 1024)),
        selfile("c.rs", &"x".repeat(40 * 1024)),
    ];
    let batches = partition_batches(&files);
    assert_eq!(batches.len(), 2);
    assert_eq!(batches[0].files.len(), 2);
    assert_eq!(batches[0].files[0].path, "a.rs");
    assert_eq!(batches[0].files[1].path, "b.rs");
    assert_eq!(batches[1].files.len(), 1);
    assert_eq!(batches[1].files[0].path, "c.rs");
    assert_eq!(batches[0].index, 1);
    assert_eq!(batches[1].index, 2);
    for b in &batches {
        assert!(b.bytes() <= BATCH_MAX_BYTES, "batch {} over cap", b.index);
    }
}

/// Why: a file bigger than the cap must never be silently dropped.
/// What: a 200 KiB file gets its own batch; a following small file starts a new one.
/// Test: this test itself.
#[test]
fn oversize_file_gets_own_batch() {
    let files = vec![
        selfile("big.rs", &"x".repeat(200 * 1024)),
        selfile("small.rs", "abc"),
    ];
    let batches = partition_batches(&files);
    assert_eq!(batches.len(), 2);
    assert_eq!(batches[0].files.len(), 1);
    assert_eq!(batches[0].files[0].path, "big.rs");
    assert_eq!(batches[1].files[0].path, "small.rs");
}

/// Why: an empty selection must not produce a spurious empty batch.
/// What: no files → no batches.
/// Test: this test itself.
#[test]
fn empty_input_yields_no_batches() {
    assert!(partition_batches(&[]).is_empty());
}

// ── Merge + dedupe ──────────────────────────────────────────────────────────

fn finding(file: &str, title: &str, severity: Severity, description: &str) -> VerifiedFinding {
    VerifiedFinding {
        title: title.to_string(),
        severity,
        dimension: "authentication & secrets".to_string(),
        file: file.to_string(),
        line: Some(1),
        evidence_quote: "q".to_string(),
        description: description.to_string(),
        business_impact: "i".to_string(),
        remediation: "r".to_string(),
        cost_effort: "low".to_string(),
    }
}

/// Why: two batches may independently surface the same `(file, title)`; the
/// higher-severity copy must win so a duplicate never masks a worse finding.
/// What: an AMBER then a RED for the same key keeps the RED; a same-severity
/// duplicate keeps the first-seen copy.
/// Test: this test itself.
#[test]
fn merge_dedupes_keeping_higher_severity() {
    let amber = finding("a.rs", "T", Severity::Amber, "first");
    let red = finding("a.rs", "T", Severity::Red, "second");
    let merged = merge_dedupe(vec![amber, red]);
    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0].severity, Severity::Red);
    assert_eq!(merged[0].description, "second", "the upgraded copy wins");

    let first = finding("b.rs", "U", Severity::Amber, "kept");
    let dup = finding("b.rs", "U", Severity::Amber, "dropped");
    let merged2 = merge_dedupe(vec![first, dup]);
    assert_eq!(merged2.len(), 1);
    assert_eq!(
        merged2[0].description, "kept",
        "same severity keeps first-seen"
    );

    // Different files/titles are never merged.
    let distinct = merge_dedupe(vec![
        finding("a.rs", "T", Severity::Red, "x"),
        finding("b.rs", "T", Severity::Red, "y"),
    ]);
    assert_eq!(distinct.len(), 2);
}

// ── Scripted provider for batch orchestration tests ─────────────────────────

/// One scripted response, consumed in call order.
enum Script {
    /// A clean structured response (finish_reason "stop").
    Ok(String),
    /// A truncated response (finish_reason "length").
    Truncated,
    /// A provider error.
    Error(String),
}

/// A provider that returns pre-scripted responses in call order — deterministic
/// for testing the retry/failure state machine without any live LLM.
struct ScriptedLlm {
    queue: Mutex<VecDeque<Script>>,
}

impl ScriptedLlm {
    fn new(scripts: Vec<Script>) -> Self {
        ScriptedLlm {
            queue: Mutex::new(scripts.into_iter().collect()),
        }
    }
}

#[async_trait]
impl LlmProvider for ScriptedLlm {
    fn name(&self) -> &str {
        "scripted"
    }
    async fn complete(&self, _req: LlmRequest) -> Result<LlmResponse, LlmError> {
        let next = self.queue.lock().unwrap().pop_front();
        match next {
            Some(Script::Ok(body)) => Ok(LlmResponse {
                text: body,
                model: "scripted".to_string(),
                input_tokens: 10,
                output_tokens: 10,
                latency_ms: 1,
                cost_usd: 0.0,
                finish_reason: Some("stop".to_string()),
            }),
            Some(Script::Truncated) => Ok(LlmResponse {
                text: "{}".to_string(),
                model: "scripted".to_string(),
                input_tokens: 10,
                output_tokens: 10,
                latency_ms: 1,
                cost_usd: 0.0,
                finish_reason: Some("length".to_string()),
            }),
            Some(Script::Error(msg)) => Err(LlmError::Transport(msg)),
            None => Err(LlmError::Transport("scripted queue exhausted".to_string())),
        }
    }
}

/// A minimal 1-finding structured response citing `file` verbatim.
fn ok_body(file: &str, title: &str, evidence: &str) -> String {
    format!(
        r#"{{"findings": [{{"title": "{title}", "severity": "red", "dimension": "authentication & secrets", "file": "{file}", "evidence_quote": "{evidence}", "description": "d", "business_impact": "i", "remediation": "r", "cost_effort": "low"}}]}}"#
    )
}

fn full_selection(files: &[SelectedFile]) -> Selection {
    Selection {
        files: files.to_vec(),
        total_files: files.len(),
        skipped: 0,
        bytes_sent: files.iter().map(|f| f.content.len()).sum(),
        dimensions_covered: vec!["authentication & secrets".to_string()],
        dimensions_absent: vec![],
    }
}

/// Why: this is the core regression test — one batch failing must never discard
/// its siblings' findings, which is exactly what the unbatched incident did.
/// What: 3 batches; batch 2 truncates on both the initial attempt AND the retry
/// (so it fails closed); batches 1 and 3 each contribute one verified finding.
/// Asserts the merged result has both survivors, batch 2 contributes none, and
/// the outcome list names batch 2 as failed while 1 and 3 are `Completed`.
/// Test: this test itself.
#[tokio::test]
async fn one_truncated_batch_others_survive() {
    let a = selfile("src/a.rs", "let secret_a = 1;\n");
    let b = selfile("src/b.rs", "let secret_b = 2;\n");
    let c = selfile("src/c.rs", "let secret_c = 3;\n");
    let selection = full_selection(&[a.clone(), b.clone(), c.clone()]);

    let batches = vec![
        FileBatch {
            index: 1,
            files: vec![a],
        },
        FileBatch {
            index: 2,
            files: vec![b],
        },
        FileBatch {
            index: 3,
            files: vec![c],
        },
    ];

    let provider: std::sync::Arc<dyn LlmProvider> = std::sync::Arc::new(ScriptedLlm::new(vec![
        Script::Ok(ok_body("src/a.rs", "Finding A", "let secret_a = 1;")),
        Script::Truncated, // batch 2, initial attempt
        Script::Truncated, // batch 2, retry
        Script::Ok(ok_body("src/c.rs", "Finding C", "let secret_c = 3;")),
    ]));

    let (findings, rejected, outcomes) =
        run_batches(provider, "stub/model", "App", &batches, 3, None, &selection).await;

    assert_eq!(rejected, 0);
    assert_eq!(
        findings.len(),
        2,
        "both surviving batches' findings present"
    );
    assert!(findings.iter().any(|f| f.title == "Finding A"));
    assert!(findings.iter().any(|f| f.title == "Finding C"));
    assert!(!findings.iter().any(|f| f.title == "Finding B"));

    assert_eq!(outcomes.len(), 3);
    assert_eq!(outcomes[0].status, BatchStatus::Completed);
    assert_eq!(outcomes[1].status, BatchStatus::Truncated);
    assert_eq!(outcomes[1].index, 2);
    assert_eq!(outcomes[1].files, vec!["src/b.rs".to_string()]);
    assert_eq!(outcomes[2].status, BatchStatus::Completed);
}

/// Why: the one-shot concise retry is the cheap-resilience path — most
/// truncations should recover here rather than failing the batch.
/// What: the initial attempt truncates; the retry succeeds; asserts `Completed`
/// with the retry's finding present.
/// Test: this test itself.
#[tokio::test]
async fn retry_recovers_from_truncation() {
    let a = selfile("src/a.rs", "let secret_a = 1;\n");
    let selection = full_selection(std::slice::from_ref(&a));
    let batches = vec![FileBatch {
        index: 1,
        files: vec![a],
    }];

    let provider: std::sync::Arc<dyn LlmProvider> = std::sync::Arc::new(ScriptedLlm::new(vec![
        Script::Truncated,
        Script::Ok(ok_body("src/a.rs", "Finding A", "let secret_a = 1;")),
    ]));

    let (findings, rejected, outcomes) =
        run_batches(provider, "stub/model", "App", &batches, 1, None, &selection).await;

    assert_eq!(rejected, 0);
    assert_eq!(findings.len(), 1);
    assert_eq!(outcomes[0].status, BatchStatus::Completed);
}

/// Why: a non-truncation failure (provider error) must be distinguished from a
/// truncation in the coverage note, and every batch failing must not panic.
/// What: two batches both error; asserts both `Unavailable` with the provider
/// error reason and zero verified findings.
/// Test: this test itself.
#[tokio::test]
async fn all_batches_can_fail_independently() {
    let a = selfile("src/a.rs", "code a\n");
    let b = selfile("src/b.rs", "code b\n");
    let selection = full_selection(&[a.clone(), b.clone()]);
    let batches = vec![
        FileBatch {
            index: 1,
            files: vec![a],
        },
        FileBatch {
            index: 2,
            files: vec![b],
        },
    ];

    let provider: std::sync::Arc<dyn LlmProvider> = std::sync::Arc::new(ScriptedLlm::new(vec![
        Script::Error("connection refused".to_string()),
        Script::Error("connection refused".to_string()),
    ]));

    let (findings, rejected, outcomes) =
        run_batches(provider, "stub/model", "App", &batches, 2, None, &selection).await;

    assert!(findings.is_empty());
    assert_eq!(rejected, 0);
    for o in &outcomes {
        match &o.status {
            BatchStatus::Unavailable(reason) => assert!(reason.contains("connection refused")),
            other => panic!("expected Unavailable, got {other:?}"),
        }
    }
}
