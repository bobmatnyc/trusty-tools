//! Tests for the trace-verdict pass, one per fail-closed branch (#6166 leg 2).
//!
//! Why: every way the verifier can fail to answer routes to the same
//! [`Verdict::Unverifiable`] sink, and a sink nothing exercises stops being
//! reached without anything going red. Each error-arm test below drives a stub
//! that produces exactly one failure and asserts BOTH the verdict and that the
//! reason names the failure — a stub whose error was dropped, or a branch that
//! quietly fell through to CONFIRMED, fails here rather than on a live
//! engagement.
//! What: the request shape, the three decided verdicts, the six fail-closed
//! arms, the counts, the fold onto findings, and the gaps line.
//! Test: included as `#[cfg(test)] mod tests` from `verdict.rs`.

use std::collections::VecDeque;
use std::sync::Mutex;

use async_trait::async_trait;

use super::*;
use crate::llm::{LlmError, LlmResponse};
use crate::report::investigate::trace::{FindingTrace, TraceAnchor, TraceLimits, TraceSet};
use crate::report::investigate::trace_client::TraceUsage;

const FILE: &str = "src/store.rs";
const TITLE: &str = "Unbounded shrink guard";

fn finding(severity: Severity) -> VerifiedFinding {
    VerifiedFinding {
        title: TITLE.to_string(),
        severity,
        dimension: "scalability".to_string(),
        file: FILE.to_string(),
        line: Some(3),
        evidence_quote: "const SHRINK_GUARD_RATIO_DIVISOR: usize = 2;".to_string(),
        description: "The divisor is a constant.".to_string(),
        business_impact: "i".to_string(),
        remediation: "r".to_string(),
        cost_effort: "low".to_string(),
        trace_verdict: String::new(),
    }
}

/// A trace record with a real anchor and one usage site.
fn traced() -> FindingTrace {
    FindingTrace {
        title: TITLE.to_string(),
        file: FILE.to_string(),
        line: Some(3),
        symbol: Some("SHRINK_GUARD_RATIO_DIVISOR".to_string()),
        anchor: Some(TraceAnchor {
            symbol: "SHRINK_GUARD_RATIO_DIVISOR".to_string(),
            file: FILE.to_string(),
            line: 3,
            signature: "pub(super) const SHRINK_GUARD_RATIO_DIVISOR: usize".to_string(),
        }),
        usages: vec![TraceUsage {
            file: FILE.to_string(),
            line: 41,
            snippet: "if len < cap / SHRINK_GUARD_RATIO_DIVISOR { self.shrink(); }".to_string(),
        }],
        usages_status: String::new(),
        call_edges: Vec::new(),
        call_edges_status: super::super::trace::CALL_EDGES_DISABLED.to_string(),
        no_trace: None,
    }
}

/// A trace record leg 1 refused.
fn refused(reason: &str) -> FindingTrace {
    FindingTrace {
        no_trace: Some(reason.to_string()),
        anchor: None,
        usages: Vec::new(),
        ..traced()
    }
}

fn trace_set(traces: Vec<FindingTrace>) -> TraceSet {
    let no_trace = traces.iter().filter(|t| t.no_trace.is_some()).count();
    TraceSet {
        index_id: Some("acme-1234abcd".to_string()),
        candidates: traces.len(),
        assembled: traces.len() - no_trace,
        no_trace,
        traces,
        limits: TraceLimits::default(),
    }
}

// ── Scripted verifier ───────────────────────────────────────────────────────

/// One scripted verifier answer, consumed in call order.
enum Script {
    /// A body returned with `finish_reason: stop`.
    Body(String),
    /// A response cut off at the output-token ceiling.
    Truncated,
    /// A transport failure.
    Error(String),
    /// A call that never returns inside [`VERDICT_TIMEOUT`].
    Hang,
}

struct ScriptedVerifier {
    queue: Mutex<VecDeque<Script>>,
    calls: Mutex<usize>,
}

impl ScriptedVerifier {
    fn new(scripts: Vec<Script>) -> Arc<Self> {
        Arc::new(ScriptedVerifier {
            queue: Mutex::new(scripts.into_iter().collect()),
            calls: Mutex::new(0),
        })
    }
}

#[async_trait]
impl LlmProvider for ScriptedVerifier {
    fn name(&self) -> &str {
        "scripted-verifier"
    }
    async fn complete(&self, _req: LlmRequest) -> Result<LlmResponse, LlmError> {
        *self.calls.lock().expect("calls") += 1;
        let next = self.queue.lock().expect("queue").pop_front();
        let (text, finish) = match next {
            Some(Script::Body(b)) => (b, "stop"),
            Some(Script::Truncated) => ("{\"verdict\":".to_string(), "length"),
            Some(Script::Error(m)) => return Err(LlmError::Transport(m)),
            Some(Script::Hang) => {
                tokio::time::sleep(VERDICT_TIMEOUT * 10).await;
                ("never".to_string(), "stop")
            }
            None => return Err(LlmError::Transport("queue exhausted".to_string())),
        };
        Ok(LlmResponse {
            text,
            model: "scripted-verifier".to_string(),
            input_tokens: 10,
            output_tokens: 10,
            latency_ms: 1,
            cost_usd: 0.0,
            finish_reason: Some(finish.to_string()),
        })
    }
}

fn verifier(scripts: Vec<Script>) -> (Verifier, Arc<ScriptedVerifier>) {
    let p = ScriptedVerifier::new(scripts);
    (
        Verifier {
            provider: p.clone(),
            model: "openrouter/anthropic/claude-haiku-4.5".to_string(),
        },
        p,
    )
}

fn body(verdict: &str, reason: &str) -> Script {
    Script::Body(format!(
        "{{\"verdict\":\"{verdict}\",\"reason\":\"{reason}\"}}"
    ))
}

/// Run the pass over one traced finding with the given scripted answers.
async fn judge(severity: Severity, scripts: Vec<Script>) -> (VerdictSet, VerifiedFinding) {
    let (v, _) = verifier(scripts);
    let traces = trace_set(vec![traced()]);
    let mut findings = vec![finding(severity)];
    let set = run_verdicts(Some(&v), &traces, &findings, "unused").await;
    apply_verdicts(&mut findings, &set, &traces);
    let f = findings.remove(0);
    (set, f)
}

// ── The request ─────────────────────────────────────────────────────────────

/// Why: the verdict is only as good as what the model was shown; a digest
/// missing the anchor or the usage snippet would be a call that judges the
/// finding on the finding alone.
/// What: the finding's title, quote, the anchor declaration, and the usage
/// snippet all reach the user message.
#[test]
fn the_request_carries_the_finding_and_its_trace() {
    let req = build_request(&finding(Severity::Red), &traced(), "mock/model");
    let msg = &req.messages[0].content;
    assert!(msg.contains(TITLE), "{msg}");
    assert!(msg.contains("const SHRINK_GUARD_RATIO_DIVISOR"), "{msg}");
    assert!(
        msg.contains("pub(super) const SHRINK_GUARD_RATIO_DIVISOR: usize"),
        "the anchor declaration must reach the prompt: {msg}"
    );
    assert!(
        msg.contains("if len < cap / SHRINK_GUARD_RATIO_DIVISOR"),
        "the usage snippet must reach the prompt: {msg}"
    );
    assert!(msg.contains("src/store.rs:41"), "{msg}");
}

/// A routing prefix is never a valid API model id.
#[test]
fn the_request_strips_the_provider_prefix() {
    let req = build_request(
        &finding(Severity::Red),
        &traced(),
        "openrouter/anthropic/claude-haiku-4.5",
    );
    assert_eq!(req.model, "anthropic/claude-haiku-4.5");
    assert_eq!(req.temperature, VERDICT_TEMPERATURE);
}

/// A usage read that failed states WHY, so the model is not told "no usages"
/// when the truth is "the usage read errored".
#[test]
fn a_failed_usage_read_reaches_the_prompt_as_its_reason() {
    let mut t = traced();
    t.usages = Vec::new();
    t.usages_status = "usages unavailable: trusty-search is not reachable".to_string();
    let req = build_request(&finding(Severity::Red), &t, "mock/model");
    assert!(
        req.messages[0]
            .content
            .contains("usages unavailable: trusty-search is not reachable"),
        "{}",
        req.messages[0].content
    );
}

/// Why: a free-text verdict would put the classification burden back on this
/// crate; the enum is what makes an off-schema answer a parse failure instead
/// of a guess.
#[test]
fn the_schema_constrains_the_verdict_token() {
    let schema = verdict_schema();
    let v = &schema.schema["properties"]["verdict"];
    assert_eq!(
        v["enum"],
        serde_json::json!(["confirmed", "cleared", "unverifiable"])
    );
    assert_eq!(
        schema.schema["properties"]["reason"]["maxLength"],
        serde_json::json!(REASON_MAX_CHARS)
    );
}

// ── The three decided verdicts ──────────────────────────────────────────────

/// Why: a confirmed finding keeps its band and gains the trace context the
/// reader would otherwise have to open the repository for.
/// What: severity untouched, marker names the anchor and the usage count.
#[tokio::test]
async fn a_confirmed_finding_carries_its_trace_context() {
    let (set, f) = judge(
        Severity::Red,
        vec![body("confirmed", "the divisor is a compile-time constant")],
    )
    .await;
    assert_eq!(set.confirmed, 1);
    assert_eq!(f.severity, Severity::Red, "confirming never moves a band");
    assert!(f.trace_verdict.starts_with("verified-by-trace"), "{f:?}");
    assert!(
        f.trace_verdict.contains("SHRINK_GUARD_RATIO_DIVISOR"),
        "the anchor symbol must reach the marker: {}",
        f.trace_verdict
    );
    assert!(
        f.trace_verdict.contains("1 usage site(s)"),
        "{}",
        f.trace_verdict
    );
    assert!(
        f.trace_verdict.contains("compile-time constant"),
        "the verifier's own reason must survive: {}",
        f.trace_verdict
    );
}

/// Why: clearing is the one arm that changes a severity, and it must change it
/// by exactly one band while leaving the finding on the page.
#[tokio::test]
async fn a_cleared_finding_drops_one_band() {
    let (set, f) = judge(
        Severity::Red,
        vec![body("cleared", "the shrink is guarded at line 41")],
    )
    .await;
    assert_eq!(set.cleared, 1);
    assert_eq!(f.severity, Severity::Amber, "RED clears to AMBER, not away");
    assert_eq!(
        f.trace_verdict,
        "cleared-by-trace: the shrink is guarded at line 41"
    );
}

/// Why: an AMBER clears into GREEN, and a GREEN renders as a one-line clean
/// signal built from the title alone. Left bare it would read as a strength the
/// investigation found — the exact way this pass could flatter a report.
#[tokio::test]
async fn a_cleared_amber_names_the_clearing_in_its_green_bullet() {
    let (_, f) = judge(Severity::Amber, vec![body("cleared", "bounded at line 41")]).await;
    assert_eq!(f.severity, Severity::Green);
    let title = super::super::metric_title(&f);
    assert_eq!(
        title, "Unbounded shrink guard — cleared-by-trace: bounded at line 41",
        "a downgraded finding must not read as a clean signal"
    );
    // An ordinary GREEN — one the trace pass never judged — keeps its bare title.
    let mut untouched = finding(Severity::Green);
    untouched.trace_verdict = String::new();
    assert_eq!(super::super::metric_title(&untouched), TITLE);
}

/// Why: an unverified finding must render exactly as it did before this pass
/// existed. Anything else lets a failing verdict pass alter the report.
#[tokio::test]
async fn an_unverifiable_finding_is_left_exactly_as_it_was() {
    let before = finding(Severity::Red);
    let (set, after) = judge(
        Severity::Red,
        vec![body("unverifiable", "the trace shows only the declaration")],
    )
    .await;
    assert_eq!(set.unverifiable, 1);
    assert_eq!(after.severity, before.severity);
    assert_eq!(
        after.trace_verdict, "",
        "no marker for an undecided verdict"
    );
    assert_eq!(after.description, before.description);
}

/// Why: the verdict annotates; it must never edit the evidence the numeric and
/// quote guardrails already verified, nor move the citation the reader checks.
#[tokio::test]
async fn clearing_never_touches_the_evidence_or_the_citation() {
    let before = finding(Severity::Red);
    let (_, after) = judge(Severity::Red, vec![body("cleared", "guarded")]).await;
    assert_eq!(after.evidence_quote, before.evidence_quote);
    assert_eq!(after.file, before.file);
    assert_eq!(after.line, before.line);
    assert_eq!(after.dimension, before.dimension);
    assert_eq!(after.title, before.title);
    assert_eq!(after.description, before.description);
    assert_eq!(after.remediation, before.remediation);
}

// ── The fail-closed arms ────────────────────────────────────────────────────

/// Why: a transport failure is the commonest one, and it must not be mistaken
/// for a finding nothing could be said about.
#[tokio::test]
async fn a_provider_error_is_unverifiable_and_names_itself() {
    let (set, f) = judge(
        Severity::Red,
        vec![Script::Error("connection reset".to_string())],
    )
    .await;
    assert_eq!(set.unverifiable, 1);
    assert_eq!(set.verdicts[0].verdict, Verdict::Unverifiable);
    assert!(
        set.verdicts[0].reason.contains("connection reset"),
        "{:?}",
        set.verdicts[0]
    );
    assert_eq!(f.severity, Severity::Red);
}

/// A response cut off at the token ceiling is not a verdict.
#[tokio::test]
async fn a_truncated_response_is_unverifiable() {
    let (set, _) = judge(Severity::Red, vec![Script::Truncated]).await;
    assert_eq!(set.unverifiable, 1);
    assert!(
        set.verdicts[0].reason.contains("truncated"),
        "{:?}",
        set.verdicts[0]
    );
}

/// Why: a refusal arrives as a 200 carrying prose, which is the same shape as
/// any other off-schema body — both must fail closed rather than parse to
/// something.
#[tokio::test]
async fn a_refusal_or_unparseable_body_is_unverifiable() {
    let (set, _) = judge(
        Severity::Red,
        vec![Script::Body(
            "I'm sorry, I can't help with analysing this code.".to_string(),
        )],
    )
    .await;
    assert_eq!(set.unverifiable, 1);
    assert!(
        set.verdicts[0].reason.contains("not a verdict object"),
        "{:?}",
        set.verdicts[0]
    );
}

/// Why: a well-formed object carrying a token this crate does not know is the
/// arm most likely to be "helpfully" mapped onto the nearest verdict.
#[tokio::test]
async fn an_unknown_verdict_token_is_unverifiable() {
    let (set, f) = judge(Severity::Red, vec![body("probably-fine", "eh")]).await;
    assert_eq!(set.unverifiable, 1);
    assert!(
        set.verdicts[0].reason.contains("probably-fine"),
        "the received token must be named: {:?}",
        set.verdicts[0]
    );
    assert_eq!(f.severity, Severity::Red);
}

/// Why: a hung provider must bound the traced set's total time; without the
/// timeout arm one stuck call stalls every remaining verdict.
#[tokio::test(start_paused = true)]
async fn a_hung_verifier_times_out_into_unverifiable() {
    let (set, _) = judge(Severity::Red, vec![Script::Hang]).await;
    assert_eq!(set.unverifiable, 1);
    assert!(
        set.verdicts[0].reason.contains("timeout"),
        "{:?}",
        set.verdicts[0]
    );
}

/// Why: leg 1's refusal is the honest reason, and spending a call on a finding
/// with nothing to judge would be budget burnt for a guaranteed non-answer.
#[tokio::test]
async fn a_no_trace_candidate_never_calls_the_model() {
    let (v, provider) = verifier(vec![body("confirmed", "should never be reached")]);
    let traces = trace_set(vec![refused("no trace: trusty-search is not reachable")]);
    let mut findings = vec![finding(Severity::Red)];
    let set = run_verdicts(Some(&v), &traces, &findings, "unused").await;
    apply_verdicts(&mut findings, &set, &traces);

    assert_eq!(*provider.calls.lock().expect("calls"), 0);
    assert_eq!(set.unverifiable, 1);
    assert_eq!(
        set.verdicts[0].reason,
        "no trace: trusty-search is not reachable"
    );
    assert_eq!(findings[0].severity, Severity::Red);
    assert_eq!(findings[0].trace_verdict, "");
}

/// Why: a run with no verifier must state the gap for every candidate, not skip
/// the pass — a silent skip renders as a report that simply had nothing to say.
#[tokio::test]
async fn without_a_verifier_every_candidate_is_unverifiable() {
    let traces = trace_set(vec![traced(), traced()]);
    let findings = vec![finding(Severity::Red)];
    let set = run_verdicts(None, &traces, &findings, "no verdict: no verifier").await;
    assert_eq!(set.traced, 2);
    assert_eq!(set.unverifiable, 2);
    assert!(set.model.is_empty());
    assert!(
        set.verdicts
            .iter()
            .all(|v| v.reason == "no verdict: no verifier")
    );
}

// ── Counts and budget ───────────────────────────────────────────────────────

/// Why: the coverage line and the gaps line both quote these counts; deriving
/// them from the verdicts once is what stops the two surfaces disagreeing.
#[tokio::test]
async fn the_counts_match_the_verdicts_and_one_call_is_made_per_trace() {
    let (v, provider) = verifier(vec![
        body("confirmed", "a"),
        body("cleared", "b"),
        body("unverifiable", "c"),
        Script::Error("boom".to_string()),
    ]);
    let traces = trace_set(vec![
        traced(),
        traced(),
        traced(),
        traced(),
        refused("no trace: no item declaration found"),
    ]);
    let findings = vec![finding(Severity::Red)];
    let set = run_verdicts(Some(&v), &traces, &findings, "unused").await;

    assert_eq!(
        *provider.calls.lock().expect("calls"),
        4,
        "one call per ASSEMBLED trace, none for the refused one"
    );
    assert_eq!(set.traced, 5, "every candidate is accounted for");
    assert_eq!(set.confirmed, 1);
    assert_eq!(set.cleared, 1);
    assert_eq!(set.unverifiable, 3, "undecided + error + no-trace");
    assert_eq!(
        set.confirmed + set.cleared + set.unverifiable,
        set.traced,
        "the three counts must partition the traced set"
    );
    assert_eq!(
        set.summary_line(),
        "1 confirmed, 1 cleared, 3 unverifiable of 5 traced"
    );
    assert_eq!(set.model, "openrouter/anthropic/claude-haiku-4.5");
}

/// A provider ignoring `maxLength` must not be able to splice a multi-line body
/// into a single-line report bullet.
#[tokio::test]
async fn a_multi_line_reason_is_collapsed_and_bounded() {
    let (set, _) = judge(
        Severity::Red,
        vec![Script::Body(
            serde_json::json!({
                "verdict": "confirmed",
                "reason": format!("line one\nline two {}", "x".repeat(400)),
            })
            .to_string(),
        )],
    )
    .await;
    let reason = &set.verdicts[0].reason;
    assert!(!reason.contains('\n'), "{reason}");
    assert!(reason.chars().count() <= REASON_MAX_CHARS, "{reason}");
}

// ── The gaps line ───────────────────────────────────────────────────────────

fn repo_with(verdicts: Option<VerdictSet>) -> super::super::RepoInvestigation {
    super::super::RepoInvestigation {
        slug: "acme".to_string(),
        name: "Acme".to_string(),
        status: super::super::InvestigationStatus::Available,
        findings: vec![],
        deps: super::super::DependencyInventory::default(),
        coverage: super::super::Coverage::default(),
        traces: None,
        verdicts,
    }
}

fn set_of(verdicts: Vec<(Verdict, &str)>) -> VerdictSet {
    VerdictSet::from_verdicts(
        verdicts
            .into_iter()
            .map(|(verdict, reason)| FindingVerdict {
                title: TITLE.to_string(),
                file: FILE.to_string(),
                verdict,
                reason: reason.to_string(),
            })
            .collect(),
        "haiku".to_string(),
    )
}

/// Why: a reader who skips the coverage section must still not read an
/// unverified finding as a disproved one.
#[test]
fn unverifiable_findings_reach_the_gaps_line() {
    let inv = super::super::Investigation {
        repos: vec![repo_with(Some(set_of(vec![
            (Verdict::Confirmed, "ok"),
            (
                Verdict::Unverifiable,
                "no trace: trusty-search is not reachable",
            ),
            (Verdict::Unverifiable, "no verdict: verifier timeout"),
        ])))],
    };
    let lines = super::super::verdict_gap_lines(&inv);
    assert_eq!(lines.len(), 1);
    assert!(
        lines[0].contains("Trace verdicts (Acme): 2 of 3"),
        "{}",
        lines[0]
    );
    assert!(
        lines[0].contains("trusty-search is not reachable"),
        "{}",
        lines[0]
    );
    assert!(lines[0].contains("verifier timeout"), "{}", lines[0]);
    assert!(
        lines[0].contains("unverified, not as disproved"),
        "{}",
        lines[0]
    );
}

/// A repository whose every traced finding got a decided verdict names no gap.
#[test]
fn a_fully_verified_repository_adds_no_gap_line() {
    let inv = super::super::Investigation {
        repos: vec![
            repo_with(Some(set_of(vec![
                (Verdict::Confirmed, "ok"),
                (Verdict::Cleared, "guarded"),
            ]))),
            repo_with(None),
        ],
    };
    assert!(super::super::verdict_gap_lines(&inv).is_empty());
}
