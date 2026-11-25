//! Unit tests for `pipeline::verify_security` (#11526).
//!
//! Why: an unreachable-but-true security finding must not be posted as
//! CONFIRMED, and the question must reach security findings only.
//! What: kind detection, schema shape, answer parsing, outcome mapping, and two
//! end-to-end rounds through `run_verification_round` with a fake verifier.
//! Test: this is the test module.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use super::*;
use crate::{
    llm::{LlmError, LlmProvider, LlmRequest, LlmResponse},
    models::{Effort, Verdict},
    pipeline::verify::run_verification_round,
    pipeline::verify_prompt::{VERIFY_SCHEMA_NAME, build_verify_request},
};

fn finding(kind: &str) -> Finding {
    let mut f = Finding::new(
        "src/admin.rs",
        kind,
        "SQL built from a string concatenation of `name`",
        "use a bound parameter",
        0.9,
        Effort::High,
    );
    f.line = Some(12);
    f.code_provable = true;
    f
}

/// A verifier that answers every request with `text` and records the requests.
struct ScriptedVerifier {
    text: String,
    seen: Mutex<Vec<LlmRequest>>,
}

#[async_trait]
impl LlmProvider for ScriptedVerifier {
    fn name(&self) -> &str {
        "scripted-verifier"
    }
    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
        if let Ok(mut seen) = self.seen.lock() {
            seen.push(req.clone());
        }
        Ok(LlmResponse {
            text: self.text.clone(),
            model: req.model.clone(),
            input_tokens: 10,
            output_tokens: 5,
            latency_ms: 1,
            cost_usd: 0.0,
            finish_reason: None,
        })
    }
}

#[test]
fn security_kind_detection() {
    assert!(is_security_finding(&finding("security")));
    assert!(is_security_finding(&finding("Security-Injection")));
    assert!(!is_security_finding(&finding("logic-error")));
}

/// The question and its schema reach a security finding and nothing else.
#[test]
fn verify_request_asks_reachability_only_for_security_findings() {
    let sec = build_verify_request("m", "+x\n", &finding("security"), None, None, None);
    assert!(sec.messages[0].content.contains("## Exploitability"));
    assert_eq!(
        sec.response_schema.as_ref().map(|s| s.name.as_str()),
        Some(SECURITY_VERIFY_SCHEMA_NAME)
    );
    assert!(sec.max_tokens >= SECURITY_VERIFY_MAX_TOKENS);

    let logic = build_verify_request("m", "+x\n", &finding("logic-error"), None, None, None);
    assert!(!logic.messages[0].content.contains("Exploitability"));
    assert_eq!(
        logic.response_schema.as_ref().map(|s| s.name.as_str()),
        Some(VERIFY_SCHEMA_NAME)
    );
}

#[test]
fn security_schema_enumerates_reachability() {
    let schema = security_verify_response_schema();
    let reach = &schema.schema["properties"]["reachability"]["enum"];
    assert_eq!(
        reach,
        &serde_json::json!(["REACHABLE", "UNREACHABLE", "UNKNOWN"])
    );
    assert!(schema.schema["properties"]["reachability_reason"].is_object());
}

#[test]
fn parse_reachability_reads_fields() {
    let a = parse_reachability(
        r#"{"judgment":"CONFIRMED","reason":"r","reachability":"UNREACHABLE","reachability_reason":"admin-only route"}"#,
    )
    .expect("parses");
    assert_eq!(a.reachability, Reachability::Unreachable);
    assert_eq!(a.rationale, "admin-only route");
    assert_eq!(parse_reachability(r#"{"judgment":"CONFIRMED"}"#), None);
    assert_eq!(parse_reachability("CONFIRMED"), None);
}

#[test]
fn unreachable_confirmed_security_finding_is_not_confirmed() {
    let answer = ReachabilityAnswer {
        reachability: Reachability::Unreachable,
        rationale: "only a hard-coded constant reaches it".to_string(),
    };
    let (outcome, note) = apply_reachability(VerifyOutcome::Confirmed, Some(&answer));
    match outcome {
        VerifyOutcome::Unverifiable { reason } => {
            assert!(reason.contains("only a hard-coded constant"), "{reason}");
        }
        other => panic!("unreachable must not stay confirmed: {other:?}"),
    }
    assert_eq!(note, None);

    let reachable = ReachabilityAnswer {
        reachability: Reachability::Reachable,
        rationale: "public handler passes `name` through".to_string(),
    };
    let (outcome, note) = apply_reachability(VerifyOutcome::Confirmed, Some(&reachable));
    assert!(matches!(outcome, VerifyOutcome::Confirmed), "{outcome:?}");
    assert!(note.is_some_and(|n| n.contains("reachable") && n.contains("public handler")));
}

/// Regression fixture (#11526): a true SQL-injection shape whose only input is
/// a constant. Before #11526 the verifier's CONFIRMED was posted as-is.
#[tokio::test]
async fn unreachable_security_finding_is_withheld_with_its_rationale() {
    let diff = "+const NAME: &str = \"admin\";\n+let q = format!(\"SELECT * FROM t WHERE n = '{}'\", NAME);\n";
    let verifier = Arc::new(ScriptedVerifier {
        text: r#"{"judgment":"CONFIRMED","reason":"concatenation is present","reachability":"UNREACHABLE","reachability_reason":"the only input is the compile-time constant NAME"}"#.to_string(),
        seen: Mutex::new(Vec::new()),
    });
    let provider: Arc<dyn LlmProvider> = verifier.clone();
    let mut findings = vec![finding("security")];
    run_verification_round(
        &provider,
        "m",
        diff,
        Verdict::Block,
        &mut findings,
        None,
        None,
        None,
    )
    .await;
    assert!(
        findings
            .iter()
            .all(|f| !matches!(f.verified, Some(VerifyOutcome::Confirmed))),
        "an unreachable security finding must not be posted CONFIRMED: {findings:?}"
    );
    let seen = verifier.seen.lock().map(|s| s.len()).unwrap_or(0);
    assert_eq!(
        seen, 1,
        "the security finding is verified in its own request"
    );
}

/// A reachable security finding stays CONFIRMED and carries the rationale.
#[tokio::test]
async fn reachable_security_finding_records_the_rationale() {
    let verifier: Arc<dyn LlmProvider> = Arc::new(ScriptedVerifier {
        text: r#"{"judgment":"CONFIRMED","reason":"present","reachability":"REACHABLE","reachability_reason":"the public handler passes the query string through"}"#.to_string(),
        seen: Mutex::new(Vec::new()),
    });
    let mut findings = vec![finding("security")];
    run_verification_round(
        &verifier,
        "m",
        "+x\n",
        Verdict::Block,
        &mut findings,
        None,
        None,
        None,
    )
    .await;
    let kept = findings
        .iter()
        .find(|f| matches!(f.verified, Some(VerifyOutcome::Confirmed)))
        .expect("a reachable security finding stays confirmed");
    assert!(
        kept.consequence.contains("public handler"),
        "{}",
        kept.consequence
    );
}

/// A CONFIRMED answer whose `reachability` is missing or not a recognised
/// value keeps the finding CONFIRMED (#11526).
#[tokio::test]
async fn confirmed_without_valid_reachability_stays_confirmed() {
    for text in [
        r#"{"judgment":"CONFIRMED","reason":"present"}"#,
        r#"{"judgment":"CONFIRMED","reason":"present","reachability":"MAYBE","reachability_reason":"x"}"#,
    ] {
        let verifier: Arc<dyn LlmProvider> = Arc::new(ScriptedVerifier {
            text: text.to_string(),
            seen: Mutex::new(Vec::new()),
        });
        let mut findings = vec![finding("security")];
        run_verification_round(
            &verifier,
            "m",
            "+x\n",
            Verdict::Block,
            &mut findings,
            None,
            None,
            None,
        )
        .await;
        assert!(
            findings
                .iter()
                .any(|f| matches!(f.verified, Some(VerifyOutcome::Confirmed))),
            "answer {text} must stay confirmed: {findings:?}"
        );
    }
}
