//! Map-reduce regressions for #9310 owner ruling 50: a chunk or synthesis
//! grade of D or F is a hard verdict floor.
//!
//! Why: the fold at `map.rs` makes a chunk graded F read BLOCK, but the
//! aggregate's low-confidence override, the wipe relax and the verifier round
//! each lowered it again; under synthesis, the verifier round relaxed a
//! synthesis F (R9 in the plan).
//! What: `chunked_diff` has one chunk per file; [`ChunkScript`] answers
//! chosen chunks with chosen replies, the rest with a clean APPROVE, and the
//! synthesis call with `synthesis` (a reply that does not parse sends the run
//! down the mechanical path). Every finding is confirmed.
//! Test: this module.

use super::*;

/// A clean chunk reply.
const CLEAN: &str = r#"{"verdict":"APPROVE","summary":"ok","findings":[]}"#;

/// A synthesis reply that does not parse: the mechanical path.
const NO_SYNTHESIS: &str = "synthesis unavailable";

/// Answers the synthesis prompt with `synthesis`, chunk `src/mr{n}.rs` with
/// the reply `chunks` pairs with `n`, and every other chunk with [`CLEAN`].
struct ChunkScript {
    chunks: Vec<(usize, String)>,
    synthesis: &'static str,
}

#[async_trait]
impl LlmProvider for ChunkScript {
    fn name(&self) -> &str {
        "chunk-script"
    }
    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
        let body: String = req.messages.iter().map(|m| m.content.as_str()).collect();
        let text = if body.contains("## PR under review") {
            self.synthesis.to_string()
        } else {
            self.chunks
                .iter()
                .find(|(n, _)| body.contains(&format!("b/src/mr{n}.rs")))
                .map_or_else(|| CLEAN.to_string(), |(_, reply)| reply.clone())
        };
        Ok(LlmResponse {
            text,
            model: req.model.clone(),
            input_tokens: 10,
            output_tokens: 400,
            latency_ms: 1,
            cost_usd: 0.0,
            finish_reason: Some("stop".to_string()),
        })
    }
}

/// A chunk reply of APPROVE graded `grade` (none when empty) with one
/// Medium finding at `confidence` on `src/mr{n}.rs:5`, quoting that line.
fn chunk_reply(n: usize, grade: &str, confidence: f32) -> String {
    let mut reply = serde_json::json!({
        "verdict": "APPROVE",
        "summary": "one note",
        "findings": [{
            "title": format!("issue in mr{n}"),
            "body": format!("`let v_{n}_5 = compute_{n}(5)` discards an error."),
            "severity": "medium",
            "confidence": confidence,
            "file": format!("src/mr{n}.rs"),
            "line": 5,
        }],
    });
    if !grade.is_empty() {
        reply["grade"] = serde_json::json!(grade);
    }
    reply.to_string()
}

/// Review [`chunked_diff`] with `chunks` and `synthesis`; every finding is
/// confirmed.
async fn run(chunks: Vec<(usize, String)>, synthesis: &'static str) -> crate::models::ReviewResult {
    let (source, _tmp) = local_source(&chunked_diff());
    let llm: Arc<dyn LlmProvider> = Arc::new(ChunkScript { chunks, synthesis });
    run_review(
        &ReviewConfig::load(None),
        input(source),
        confirmed_deps(llm),
    )
    .await
}

/// #9310 ruling 50, R8: a chunk APPROVE graded F with one confirmed Medium at
/// 0.60 folds to BLOCK, and `aggregate_verdict`'s low-confidence override
/// relaxed the aggregate to APPROVE. The worst chunk floor holds BLOCK.
#[tokio::test]
async fn mapreduce_chunk_graded_f_with_low_confidence_finding_reads_block() {
    let result = run(vec![(0, chunk_reply(0, "F", 0.60))], NO_SYNTHESIS).await;
    assert_eq!(result.findings.len(), 1, "{:?}", result.withheld_findings);
    assert_eq!(result.verdict, Verdict::Block, "{result:?}");
    assert_eq!(result.grade.as_deref(), Some("F"));
}

/// #9310 ruling 50, the wipe path: chunk 0 APPROVE graded F whose only finding
/// cites a file outside the diff is wiped and relaxed to APPROVE; chunk 1's
/// confirmed Medium at 0.60 survives, so the withheld mapping keeps the
/// gates' APPROVE. The floor is read before the wipe.
#[tokio::test]
async fn mapreduce_wiped_f_chunk_beside_a_surviving_chunk_reads_block() {
    let wiped = r#"{"verdict":"APPROVE","grade":"F","summary":"unsound","findings":[{"title":"bad import","body":"`helper` is never defined.","severity":"high","confidence":0.9,"file":"src/not_in_diff.rs","line":1,"code_provable":true}]}"#;
    let result = run(
        vec![(0, wiped.to_string()), (1, chunk_reply(1, "", 0.60))],
        NO_SYNTHESIS,
    )
    .await;
    assert_eq!(result.findings.len(), 1, "{:?}", result.findings);
    assert_eq!(result.findings[0].file, "src/mr1.rs");
    assert!(!result.withheld_findings.is_empty(), "{result:?}");
    assert_eq!(result.verdict, Verdict::Block, "{result:?}");
    assert_eq!(
        result.verdict_status,
        Some(crate::models::VerdictStatus::Parsed)
    );
}

/// #9310 ruling 50, the gate round: a chunk APPROVE graded F with one
/// confident Medium aggregates to BLOCK, and `rederive_verdict` path (a2)
/// softened it to REQUEST_CHANGES.
#[tokio::test]
async fn mapreduce_chunk_graded_f_with_confirmed_medium_stays_block() {
    let result = run(vec![(0, chunk_reply(0, "F", 0.95))], NO_SYNTHESIS).await;
    assert_eq!(result.findings.len(), 1, "{:?}", result.withheld_findings);
    assert_eq!(result.verdict, Verdict::Block, "{result:?}");
    assert_eq!(result.grade.as_deref(), Some("F"));
}

/// #9310 control: the same chunk graded C- keeps today's relaxed APPROVE. A
/// floor on every grade's verdict holds it at APPROVE*.
#[tokio::test]
async fn mapreduce_chunk_graded_c_minus_keeps_the_relaxed_result() {
    let result = run(vec![(0, chunk_reply(0, "C-", 0.60))], NO_SYNTHESIS).await;
    assert_eq!(result.findings.len(), 1, "{:?}", result.withheld_findings);
    assert_eq!(result.verdict, Verdict::Approve, "{result:?}");
}

/// #9310 owner answer Q2: when synthesis answers, only its grade floors. A
/// synthesis APPROVE graded A beside a chunk graded F with a low-confidence
/// finding stays APPROVE.
#[tokio::test]
async fn synthesis_answering_ignores_the_chunk_floor() {
    let synthesis = r#"{"verdict":"APPROVE","grade":"A","summary":"looks fine."}"#;
    let result = run(vec![(0, chunk_reply(0, "F", 0.60))], synthesis).await;
    assert_eq!(result.findings.len(), 1, "{:?}", result.withheld_findings);
    assert_eq!(result.verdict, Verdict::Approve, "{result:?}");
}

/// #9310 ruling 50, R9: a synthesis APPROVE graded F folds to BLOCK, and the
/// verifier round relaxed it to APPROVE on one confirmed low-confidence
/// finding.
#[tokio::test]
async fn synthesis_f_with_a_confirmed_low_confidence_finding_reads_block() {
    let synthesis = r#"{"verdict":"APPROVE","grade":"F","summary":"unsound."}"#;
    let result = run(vec![(0, chunk_reply(0, "", 0.60))], synthesis).await;
    assert_eq!(result.findings.len(), 1, "{:?}", result.withheld_findings);
    assert_eq!(result.verdict, Verdict::Block, "{result:?}");
    assert_eq!(result.grade.as_deref(), Some("F"));
}

/// #9310 control: a synthesis REQUEST_CHANGES graded B reads D+ in
/// `ReducedReview::grade` (clamped to its verdict). The floor reads the raw
/// B, so the verifier round still relaxes the review to APPROVE, as today.
#[tokio::test]
async fn synthesis_rc_graded_b_still_relaxes() {
    let synthesis = r#"{"verdict":"REQUEST_CHANGES","grade":"B","summary":"one note."}"#;
    let result = run(vec![(0, chunk_reply(0, "", 0.60))], synthesis).await;
    assert_eq!(result.findings.len(), 1, "{:?}", result.withheld_findings);
    assert_eq!(result.verdict, Verdict::Approve, "{result:?}");
}

/// #9310 fix round 1 (HIGH 2): a chunk APPROVE graded F whose only finding is
/// withheld, synthesis off. The aggregate relaxed to APPROVE, so the withheld
/// mapping read APPROVE / `all_withheld` and the floor then raised it to
/// BLOCK beside that label. The chunk floor now folds into the reviewers'
/// verdict, so the review reads Q1's REQUEST_CHANGES / `suppressed_reject`,
/// whether no verifier ran or the verifier refuted the finding.
#[tokio::test]
async fn mapreduce_chunk_f_with_its_only_finding_withheld_is_suppressed_reject() {
    let unverified = deps(Arc::new(ChunkScript {
        chunks: vec![(0, chunk_reply(0, "F", 0.60))],
        synthesis: NO_SYNTHESIS,
    }));
    let mut refuted = deps(Arc::new(ChunkScript {
        chunks: vec![(0, chunk_reply(0, "F", 0.60))],
        synthesis: NO_SYNTHESIS,
    }));
    refuted.verifier = Some(Arc::new(RefutingVerifier {
        refute: "discards an error",
    }));
    for (case, review_deps) in [("no verifier", unverified), ("refuted", refuted)] {
        let (source, _tmp) = local_source(&chunked_diff());
        let result = run_review(&ReviewConfig::load(None), input(source), review_deps).await;
        assert!(result.findings.is_empty(), "{case}: {:?}", result.findings);
        assert_eq!(result.withheld_findings.len(), 1, "{case}: {result:?}");
        assert_eq!(
            result.verdict,
            Verdict::RequestChanges,
            "{case}: {result:?}"
        );
        assert_eq!(
            result.verdict_status,
            Some(crate::models::VerdictStatus::SuppressedReject),
            "{case}"
        );
    }
}
