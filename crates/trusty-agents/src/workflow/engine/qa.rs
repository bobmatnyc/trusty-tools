//! QA agent envelope parsing (claude-mpm parity).
//!
//! Why: When the QA agent emits structured JSON we can gate workflow
//! advancement on `status` and capture exact pass/fail counts for perf
//! records. Falling back to opaque text keeps every existing workflow working
//! unchanged.
//! What: Best-effort JSON extraction — accepts a bare JSON document, a fenced
//! ```json``` block, or a JSON object embedded anywhere in free text.
//! Test: `qa_envelope_parses_status_and_counts`,
//! `qa_envelope_returns_none_for_free_text`.

/// Parsed QA agent envelope (claude-mpm parity).
///
/// Why: When the QA agent emits structured output we can gate workflow
/// advancement on `status` and capture exact `passed`/`failed` counts for
/// perf records. Falling back to opaque text keeps every existing workflow
/// working unchanged.
/// What: Holds the parsed status plus optional pass/fail counts and details.
/// Test: `qa_envelope_parses_status_and_counts`,
/// `qa_envelope_returns_none_for_free_text`.
#[derive(Debug, Clone)]
pub(crate) struct QaEnvelope {
    pub status: QaStatus,
    pub passed: Option<u64>,
    pub failed: Option<u64>,
    pub details: Option<String>,
}

/// Pass/fail outcome parsed from a QA envelope's `status` field.
///
/// Why: The engine branches on this to decide whether to queue a one-shot
/// retry feedback block for the next code phase.
/// What: Two-variant enum mapped from a set of accepted status strings.
/// Test: `qa_envelope_parses_status_and_counts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QaStatus {
    Pass,
    Fail,
}

/// Parse a QA agent envelope out of raw agent output.
///
/// Why: See module docs — gates workflow advancement and records counts.
/// What: Tries each JSON candidate shape in order; returns the first that has
/// a recognized `status`. Returns `None` for free text or JSON missing
/// `status`. Never panics.
/// Test: `qa_envelope_parses_status_and_counts`,
/// `qa_envelope_returns_none_for_free_text`.
pub(crate) fn parse_qa_envelope(raw: &str) -> Option<QaEnvelope> {
    let candidates = extract_json_candidates(raw);
    for candidate in candidates {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&candidate) {
            let status = value.get("status").and_then(|s| s.as_str())?;
            let status = match status.trim().to_ascii_lowercase().as_str() {
                "pass" | "passed" | "ok" | "success" => QaStatus::Pass,
                "fail" | "failed" | "error" => QaStatus::Fail,
                _ => return None,
            };
            let passed = value.get("passed").and_then(|v| v.as_u64());
            let failed = value.get("failed").and_then(|v| v.as_u64());
            // Prefer `details`; fall back to joined `errors`; then `summary`.
            let details = value
                .get("details")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .or_else(|| {
                    value.get("errors").and_then(|v| v.as_array()).map(|arr| {
                        arr.iter()
                            .filter_map(|e| e.as_str())
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                })
                .or_else(|| {
                    value
                        .get("summary")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                });
            return Some(QaEnvelope {
                status,
                passed,
                failed,
                details,
            });
        }
    }
    None
}

/// Extract JSON candidate substrings from a raw QA agent output blob.
///
/// Why: Agents may emit a bare JSON document, a fenced ```json``` block, or
/// JSON embedded in markdown narration. We try each shape in order so the
/// most disciplined output wins, but free-text outputs degrade gracefully
/// to `None`.
/// What: Returns up to three candidates: the trimmed input, the contents of
/// the first ```json``` fence, and the substring from the first `{` to the
/// last `}`.
/// Test: `qa_envelope_parses_status_and_counts`.
fn extract_json_candidates(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    let trimmed = raw.trim();
    if !trimmed.is_empty() {
        out.push(trimmed.to_string());
    }
    // Fenced ```json``` block.
    if let Some(start) = raw.find("```json") {
        let after = &raw[start + "```json".len()..];
        if let Some(end) = after.find("```") {
            let body = after[..end].trim();
            if !body.is_empty() {
                out.push(body.to_string());
            }
        }
    }
    // First-{ to last-} embedded scan as a last resort.
    if let (Some(s), Some(e)) = (raw.find('{'), raw.rfind('}'))
        && e > s
    {
        out.push(raw[s..=e].to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: Fix 2 — QA envelope parser must extract status + counts from a
    /// fenced ```json``` block, a bare JSON document, AND a JSON object
    /// embedded in surrounding markdown. Counts must round-trip.
    /// What: Three parse shapes asserted against a single canonical envelope.
    #[test]
    fn qa_envelope_parses_status_and_counts() {
        // Bare JSON
        let env = parse_qa_envelope(r#"{"status":"pass","passed":42,"failed":0,"summary":"ok"}"#)
            .expect("parse bare json");
        assert_eq!(env.status, QaStatus::Pass);
        assert_eq!(env.passed, Some(42));
        assert_eq!(env.failed, Some(0));

        // Fenced
        let env = parse_qa_envelope(
            "Here is the result:\n```json\n{\"status\":\"fail\",\"passed\":3,\"failed\":2,\"errors\":[\"e1\",\"e2\"]}\n```\n",
        )
        .expect("parse fenced json");
        assert_eq!(env.status, QaStatus::Fail);
        assert_eq!(env.passed, Some(3));
        assert_eq!(env.failed, Some(2));
        assert!(env.details.unwrap().contains("e1"));

        // Embedded
        let env = parse_qa_envelope(
            "I ran the suite. Result: {\"status\":\"fail\",\"failed\":1,\"details\":\"boom\"} done.",
        )
        .expect("parse embedded json");
        assert_eq!(env.status, QaStatus::Fail);
        assert_eq!(env.failed, Some(1));
        assert_eq!(env.details.as_deref(), Some("boom"));
    }

    /// Why: Fix 2 backward compatibility — free-text QA output must NOT
    /// produce a parsed envelope, so the workflow continues exactly as
    /// before.
    #[test]
    fn qa_envelope_returns_none_for_free_text() {
        assert!(parse_qa_envelope("All tests passed!").is_none());
        assert!(parse_qa_envelope("35/35 passed").is_none());
        assert!(parse_qa_envelope("").is_none());
        // JSON without `status` is also None (we require it).
        assert!(parse_qa_envelope(r#"{"passed":5}"#).is_none());
    }
}
