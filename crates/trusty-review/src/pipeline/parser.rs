//! Verdict and findings parser for LLM review responses.
//!
//! Why: structured output (via `response_schema` forced output) makes the
//! LLM return a clean JSON object directly, eliminating the fail-safe APPROVE
//! problem.  Free-text parsing is retained as a fallback for transport errors
//! and for callers that do not use forced structured output.
//!
//! What: exposes `parse_review_response` which tries three strategies in order:
//!
//!  1. Direct JSON parse — tries `serde_json::from_str` on the full body.
//!     This succeeds when forced structured output is active (Bedrock tool-use
//!     or OpenRouter json_schema) and the response IS the JSON object.
//!  2. JSON-block extraction — looks for a ```json ... ``` fenced block at the
//!     end of the response and deserialises it (legacy free-text path).
//!  3. Verdict-keyword scan — scans the last 20% of the body for one of the
//!     known board grade tokens (BLOCK, REQUEST_CHANGES, APPROVE*, APPROVE,
//!     UNKNOWN) per spec REV-112.
//!
//! If strategies 1 and 2 fail the response is fail-safe UNKNOWN, whether or not
//! the keyword scan recovered a token — see the fail-CLOSED note below.
//!
//! ## Fail-CLOSED posture (#1241 — supersedes spec REV-130)
//! Spec REV-130 originally specified a fail-OPEN APPROVE here: any parse/LLM
//! failure would silently APPROVE so a pipeline failure never blocked a merge.
//! Ticket #1241 supersedes that decision (ticket > spec precedence): a silent
//! APPROVE on unparseable or truncated model output is a *safety hole* — it
//! posts a green GitHub check for a review that never actually happened.  The
//! fail-safe is now fail-CLOSED: `verdict = UNKNOWN`, which surfaces a clear
//! "could not review" state and never posts a green merge-approval.  See
//! `docs/specs/` REV-130 (marked SUPERSEDED) for the rationale.
//!
//! ## The keyword scan no longer passes a verdict through (#4491)
//! Strategy 3 used to return the scanned verdict with an empty findings list and
//! `is_fail_safe = false`, so a lost findings payload rendered as `Findings:
//! none` — indistinguishable from a clean review.  It now feeds the fail-safe
//! reason instead of the verdict: the scanned token is reported to the operator,
//! never trusted as a review outcome.  The most common cause of that lost
//! payload — a double-encoded `findings` string — is now decoded rather than
//! rejected, so the evidence usually survives in the first place.
//!
//! Test: `parse_direct_json_happy_path`, `parse_json_block_happy_path`,
//! `parse_verdict_keyword_fallback_approve_star`,
//! `parse_fail_safe_unknown_on_empty_response`,
//! `parse_fail_safe_unknown_on_malformed_json`,
//! `parse_double_encoded_findings_are_recovered`,
//! `parse_unparseable_findings_is_loud_not_silently_empty`.

use serde::{Deserialize, Deserializer, de};
use tracing::{debug, warn};

use crate::models::{Effort, Finding, FindingCategory, Verdict};

// ─── Wire types (JSON block deserialization) ──────────────────────────────────

/// Deserialized JSON output block from the LLM reviewer.
///
/// Why: the LLM is instructed to end its response with this JSON block; we
/// deserialise it directly for structured extraction.
/// What: mirrors the output schema in `prompt::reviewer_system_prompt`.
/// Unknown fields are ignored for forward-compatibility.  The `grade` field is
/// new in 0.3.4 (#732); it is optional with `serde(default)` so old responses
/// without it still parse cleanly.
/// Test: `parse_json_block_happy_path`.
#[derive(Debug, Deserialize)]
struct LlmOutputBlock {
    verdict: String,
    #[serde(default)]
    grade: String,
    #[serde(default)]
    #[allow(dead_code)] // Deserialized for schema compliance; not used programmatically.
    grade_justification: String,
    #[serde(default)]
    summary: String,
    // #4491: accepts a double-encoded findings string as well as a real array.
    #[serde(default, deserialize_with = "deserialize_findings")]
    findings: Vec<LlmFinding>,
}

/// Prefix of the error a double-encoded findings string raises when it does
/// not decode; `describe_block_error` lets it through (#9310).
const FINDINGS_DECODE_ERROR: &str = "the double-encoded findings string does not decode";

/// Visitor for the two shapes a model actually emits for `findings`.
///
/// Why: a provider occasionally returns the findings array **double-encoded** —
/// a JSON *string* whose contents are the array — instead of the array itself
/// (#4491). #9310: a visitor, not an untagged enum, so a bad finding keeps its
/// own serde error ("missing field `body`") instead of "did not match any
/// variant".
/// What: `visit_seq` reads a real array; `visit_str` decodes the string once
/// more, mapping an empty string to no findings. Any other JSON type is an
/// invalid-type error, as with the untagged enum before.
/// Test: `parse_double_encoded_findings_are_recovered`,
/// `parse_finding_without_body_fails_safe_naming_body`.
struct FindingsVisitor;

impl<'de> de::Visitor<'de> for FindingsVisitor {
    type Value = Vec<LlmFinding>;

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("a findings array or a JSON string holding one")
    }

    fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let mut findings = Vec::with_capacity(seq.size_hint().unwrap_or(0));
        while let Some(finding) = seq.next_element()? {
            findings.push(finding);
        }
        Ok(findings)
    }

    fn visit_str<E: de::Error>(self, raw: &str) -> Result<Self::Value, E> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Ok(Vec::new());
        }
        warn!("findings arrived double-encoded as a JSON string — decoding again (#4491)");
        // #9310: the inner error is described, never echoed, as it can quote the reply.
        serde_json::from_str(trimmed).map_err(|e| {
            E::custom(format!(
                "{FINDINGS_DECODE_ERROR}: {}",
                describe_block_error(&e)
            ))
        })
    }
}

/// Deserialize `findings`, tolerating one layer of double encoding (#4491).
///
/// Why: dropping the whole block over an encoding quirk cost PR #4483 three
/// findings that were reported as `Findings: none`.
/// What: delegates to [`FindingsVisitor`]. A second decode failure is
/// propagated as a deserialization error so the caller fails CLOSED rather
/// than reporting zero findings for a payload that carried some.
/// Test: `parse_double_encoded_findings_are_recovered`,
/// `parse_unparseable_findings_is_loud_not_silently_empty`.
fn deserialize_findings<'de, D>(deserializer: D) -> Result<Vec<LlmFinding>, D::Error>
where
    D: Deserializer<'de>,
{
    deserializer.deserialize_any(FindingsVisitor)
}

/// A single finding from the LLM JSON output block.
///
/// Why: the LLM emits findings as structured JSON; we convert them to the
/// internal `Finding` type.
/// What: mirrors the finding schema in the system prompt.  Only `body` is
/// required; every other field defaults gracefully, and a blank `title` is
/// derived from `body` in `convert_llm_finding` (#9310).  `category` is new in
/// #1359 (back gate); it is `#[serde(default)]` (→ `Correctness`) so responses
/// from models that do not emit it — and every pre-#1359 fixture — still parse.
/// Test: covered transitively by `parse_json_block_happy_path` and
/// `parse_method_conformance_finding_category` in `parser_tests`.
#[derive(Debug, Deserialize)]
struct LlmFinding {
    // #9310: Claude in tool-choice auto omits `title`; it is derived, never required.
    #[serde(default)]
    title: String,
    body: String,
    #[serde(default)]
    severity: String,
    #[serde(default)]
    confidence: f32,
    #[serde(default)]
    file: String,
    #[serde(default)]
    line: Option<u32>,
    /// Finding axis: `"correctness"` (default) or `"method-conformance"` (#1359).
    #[serde(default)]
    category: FindingCategory,
    /// Brief failure consequence — what goes wrong if unaddressed (#1416).
    ///
    /// `#[serde(default)]` → `""` so models that omit it — and every pre-#1416
    /// fixture — still parse.
    #[serde(default)]
    consequence: String,
    /// Exact replacement code for a committable GitHub `suggestion` block (#1415).
    ///
    /// `#[serde(default)]` → `None` so models that omit it — and every pre-#1415
    /// fixture — still parse.
    #[serde(default)]
    suggested_replacement: Option<String>,
    /// Exact spec/ticket/test-plan source grounding this finding (#1419).
    ///
    /// `#[serde(default)]` → `None` so models that omit it — and every pre-#1419
    /// fixture — still parse.
    #[serde(default)]
    source_citation: Option<String>,
    /// Core-algorithmic-correctness flag (#PR84): `true` when the model asserts
    /// this is a logic/data/security bug provable from the diff itself (not
    /// external framework/platform speculation).  `#[serde(default)]` → `false`
    /// (fail-closed) so models that omit it — and every pre-#PR84 fixture — still
    /// parse and are treated as non-escalation-eligible unless cited.
    #[serde(default)]
    code_provable: bool,
}

// ─── Parsed output ────────────────────────────────────────────────────────────

/// The structured result of parsing a raw LLM review response.
///
/// Why: the pipeline receives a `ParsedReview` and populates a `ReviewResult`
/// from it; keeping the parsed form separate from the final result allows the
/// pipeline to apply confidence-threshold gates before committing the result.
/// What: contains the parsed verdict, grade, summary, and findings list, plus a
/// flag indicating whether the result was produced by the fail-safe path.
/// The `grade` is `None` when the LLM omitted or produced an unparseable grade;
/// the runner falls back to `default_grade_for_verdict` in that case.
/// Test: all parser tests assert `ParsedReview` fields.
#[derive(Debug, Clone)]
pub struct ParsedReview {
    /// Parsed or fail-safe verdict.
    pub verdict: Verdict,
    /// Letter grade from the LLM (A+ through F), or `None` if not provided.
    pub grade: Option<String>,
    /// Pre-floor synthesis letter grade (#1665 item 3) — the grade derived from
    /// the LLM's RAW synthesis verdict BEFORE the two-tier floor was applied.
    /// `None` for non-synthesis reviews or when synthesis is disabled/failed.
    /// When `grade_pre_floor != grade`, the floor changed the verdict; when equal,
    /// no flooring occurred.
    pub grade_pre_floor: Option<String>,
    /// One-line summary extracted from the JSON block, or empty string.
    pub summary: String,
    /// Parsed findings (may be empty).
    pub findings: Vec<Finding>,
    /// True if the parser failed and fell back to the fail-safe UNKNOWN default.
    pub is_fail_safe: bool,
    /// Human-readable reason for the fail-safe, if `is_fail_safe` is true.
    pub fail_safe_reason: Option<String>,
}

impl ParsedReview {
    /// Construct a fail-safe result with verdict UNKNOWN (fail-CLOSED).
    ///
    /// Why: ticket #1241 supersedes spec REV-130's fail-OPEN APPROVE.  A silent
    /// APPROVE on unparseable/truncated model output posts a green GitHub check
    /// for a review that never happened — a safety hole.  Failing CLOSED to
    /// UNKNOWN surfaces a clear "could not review" state that is never treated as
    /// a merge-approval downstream (see `post.rs` / the webhook finalize path).
    /// What: sets `verdict = Unknown`, `findings = []`, `is_fail_safe = true`.
    /// Test: `parse_fail_safe_unknown_on_empty_response`.
    pub fn fail_safe(reason: impl Into<String>) -> Self {
        Self {
            verdict: Verdict::Unknown,
            grade: None,
            grade_pre_floor: None,
            summary: String::new(),
            findings: Vec::new(),
            is_fail_safe: true,
            fail_safe_reason: Some(reason.into()),
        }
    }
}

// ─── Main parser ──────────────────────────────────────────────────────────────

/// Parse a raw LLM review response into a structured `ParsedReview`.
///
/// Why: the pipeline cannot use the raw text directly; structured data is needed
/// to drive the verdict, findings post-processing, and telemetry.
///
/// What: tries three strategies in priority order:
///   1. Direct JSON parse — succeeds when forced structured output (Bedrock
///      tool-use / OpenRouter json_schema) is active; body IS the clean JSON.
///   2. JSON-block extraction — legacy free-text path with fenced JSON block.
///   3. Verdict-keyword scan — last-resort spec REV-112 fallback, which now only
///      annotates the fail-safe reason (#4491).
///
/// If strategies 1 and 2 fail, returns fail-safe UNKNOWN (fail-CLOSED) — the
/// findings are unrecoverable at that point, and a verdict rendered without them
/// reads as a clean review (#4491).  Ticket #1241 supersedes spec REV-130: the
/// fail-safe is UNKNOWN, not APPROVE.
///
/// Test: `parse_direct_json_happy_path`, `parse_json_block_happy_path`,
/// `parse_verdict_keyword_fallback_approve_star`,
/// `parse_fail_safe_unknown_on_empty_response`,
/// `parse_double_encoded_findings_are_recovered`.
pub fn parse_review_response(body: &str) -> ParsedReview {
    if body.trim().is_empty() {
        warn!("LLM returned empty response — applying fail-safe UNKNOWN (fail-closed, #1241)");
        return ParsedReview::fail_safe("empty LLM response");
    }

    // #9310: the first serde error from strategy 1 or 2, named in the reason.
    let mut block_error = None;

    // Strategy 1: direct JSON parse (structured output path).
    // When response_schema is used, the provider returns only the JSON object.
    match try_parse_direct_json(body) {
        Some(Ok(parsed)) => {
            debug!(verdict = ?parsed.verdict, findings = parsed.findings.len(), "parsed via direct JSON (structured output)");
            return parsed;
        }
        Some(Err(e)) => block_error = Some(e),
        None => {}
    }

    // Strategy 2: JSON block (legacy free-text path).
    match try_parse_json_block(body) {
        Some(Ok(parsed)) => {
            debug!(verdict = ?parsed.verdict, findings = parsed.findings.len(), "parsed via JSON block");
            return parsed;
        }
        Some(Err(e)) => {
            block_error.get_or_insert(e);
        }
        None => {}
    }

    // Strategy 3: the structured payload did not parse, so the FINDINGS are gone.
    // #4491: a keyword-scanned verdict beside an empty findings list is
    // byte-for-byte indistinguishable from a genuinely clean review, so the
    // scanned token is reported as context in the fail-safe reason and never as
    // the review's own verdict.
    let mut reason = match scan_verdict_keyword(body) {
        Some(verdict) => format!(
            "findings could not be parsed from the LLM response; the trailing \
             keyword scan read {verdict}, which is not trusted as a review outcome \
             (spec REV-112 fallback, #4491)"
        ),
        None => "no parseable verdict or findings in LLM response".to_string(),
    };
    // #9310: a broken json fence is the model's own object; name it. The reply
    // fails closed either way, and no other object in it is ever trusted.
    if has_malformed_json_fence(body) {
        reason = format!(
            "a json fence in the LLM response holds no valid review object (#9310); {reason}"
        );
    }
    // #9310: name the serde cause and position; never the reply text.
    if let Some(e) = block_error {
        reason = format!(
            "{reason}; the review object did not deserialize: {}",
            describe_block_error(&e)
        );
    }
    warn!(
        body_len = body.len(),
        reason,
        "failed to parse the LLM response — applying fail-safe UNKNOWN (fail-closed, #4491)"
    );
    ParsedReview::fail_safe(reason)
}

// ─── Strategy 1: Direct JSON parse (structured output) ───────────────────────

/// Try to deserialize the entire response body as a `LlmOutputBlock`.
///
/// Why: when forced structured output is active (Bedrock tool-use / OpenRouter
/// json_schema), the provider guarantees `LlmResponse.text` contains only the
/// clean JSON object — no fence, no surrounding prose.  Parsing it directly
/// avoids the fragile fence-stripping logic entirely.
/// What: trims whitespace and calls `serde_json::from_str` on the full body.
/// Returns `None` when the body does not start with `{`, and `Some(Err)` with
/// the serde error when it does but is not a valid `LlmOutputBlock` (#9310);
/// either way the caller falls through to the fence-based strategy.
/// Test: `parse_direct_json_happy_path`,
/// `parse_direct_json_request_changes_with_findings`,
/// `parse_block_without_verdict_fails_safe_naming_verdict`.
fn try_parse_direct_json(body: &str) -> Option<Result<ParsedReview, serde_json::Error>> {
    let trimmed = body.trim();
    // Only attempt if it looks like a JSON object (starts with '{').
    if !trimmed.starts_with('{') {
        return None;
    }
    Some(serde_json::from_str::<LlmOutputBlock>(trimmed).map(parsed_from_block))
}

/// Build the `ParsedReview` for a deserialized review object.
///
/// Why: strategies 1 and 2 share one conversion, so the verdict rule and the
/// derived-title count cannot drift between them.
/// What: an unrecognised verdict token becomes UNKNOWN, never APPROVE
/// (fail-CLOSED, #1241). Findings with a blank `title` are counted and the
/// count is logged once per parsed reply, without content (#9310).
/// Test: `parse_untitled_finding_keeps_verdict_and_derives_title`,
/// `parse_fail_safe_unknown_on_unparseable_verdict`.
fn parsed_from_block(block: LlmOutputBlock) -> ParsedReview {
    let verdict = parse_verdict_string(&block.verdict).unwrap_or(Verdict::Unknown);
    let grade = extract_grade_field(&block.grade);
    // #9310: count derived titles so reviewer schema drift stays visible.
    let derived = block
        .findings
        .iter()
        .filter(|f| f.title.trim().is_empty())
        .count();
    if derived > 0 {
        warn!(
            derived_titles = derived,
            findings = block.findings.len(),
            "reviewer findings arrived without a title; derived each from its body (#9310)"
        );
    }
    let findings = block
        .findings
        .into_iter()
        .map(convert_llm_finding)
        .collect();
    ParsedReview {
        verdict,
        grade,
        grade_pre_floor: None,
        summary: block.summary,
        findings,
        is_fail_safe: false,
        fail_safe_reason: None,
    }
}

// ─── Strategy 2: JSON block (legacy free-text) ────────────────────────────────

/// Try to extract and deserialize the trailing ```json ... ``` block.
///
/// Why: the structured output format is the preferred extraction path; it
/// provides the full findings list with confidence scores.
/// What: scans for the last occurrence of ```json ... ``` in the response;
/// if found, deserialises the JSON and converts findings to the internal type.
/// Returns `None` if no closed ```json fence is found, and `Some(Err)` with the
/// serde error when the fenced text is not a valid review object (#9310).
/// Test: `parse_json_block_happy_path`, `parse_json_block_handles_fence_variants`,
/// `parse_finding_without_body_fails_safe_naming_body`.
fn try_parse_json_block(body: &str) -> Option<Result<ParsedReview, serde_json::Error>> {
    // Find the last ```json fence.
    let fence_start = body.rfind("```json")?;
    let after_fence = &body[fence_start + 7..]; // skip ```json

    // Find the closing fence.
    let fence_end = after_fence.find("```")?;
    let json_text = after_fence[..fence_end].trim();

    Some(serde_json::from_str::<LlmOutputBlock>(json_text).map(parsed_from_block))
}

/// Describe a review-object serde error without quoting the reply (#9310).
///
/// Why: the fail-safe reason reaches `result.error` and the PR check, so it
/// must name the cause — but serde messages for a wrong type or an unknown
/// enum variant quote the offending value, which is reply content.
/// What: keeps the message for syntax and EOF errors (fixed serde_json text)
/// and for `missing field`/`duplicate field` errors (the name is a struct
/// field, never input) and the double-encoded findings error (itself built
/// here). Any other data error becomes a fixed phrase. Line and column are
/// always appended.
/// Test: `parse_block_without_verdict_fails_safe_naming_verdict`,
/// `parse_error_reason_never_echoes_reply_content`.
fn describe_block_error(e: &serde_json::Error) -> String {
    let location = format!(" at line {} column {}", e.line(), e.column());
    let full = e.to_string();
    let message = full.strip_suffix(location.as_str()).unwrap_or(&full);
    let message = match e.classify() {
        serde_json::error::Category::Syntax | serde_json::error::Category::Eof => message,
        serde_json::error::Category::Data => {
            safe_data_message(message).unwrap_or("a field holds an invalid type or value")
        }
        serde_json::error::Category::Io => "an I/O error",
    };
    format!("{message}{location}")
}

/// The part of a serde data-error message that names only schema, if any.
///
/// Why/What: a "missing field" or "duplicate field" message is cut after the
/// quoted field name, which must be a plain identifier; a decode error raised by
/// [`FindingsVisitor`] is kept whole, since its detail is already described.
/// Anything else is `None` (#9310).
/// Test: `parse_error_reason_never_echoes_reply_content`.
fn safe_data_message(message: &str) -> Option<&str> {
    if message.starts_with(FINDINGS_DECODE_ERROR) {
        return Some(message);
    }
    for prefix in ["missing field `", "duplicate field `"] {
        if let Some(rest) = message.strip_prefix(prefix) {
            let name = rest.split('`').next()?;
            let is_ident =
                !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
            return is_ident.then(|| &message[..prefix.len() + name.len() + 1]);
        }
    }
    None
}

/// Longest derived finding title, in chars, ellipsis included (#9310).
const DERIVED_TITLE_MAX_CHARS: usize = 120;

/// Title for an untitled finding whose `body` is blank too (#9310).
const UNTITLED_FINDING_PLACEHOLDER: &str = "Untitled finding";

/// Derive a finding title from its `body` (#9310).
///
/// Why: Claude in Bedrock tool-choice auto omits `title` on many findings, and
/// a required `title` failed the whole reply to UNKNOWN.
/// What: the first sentence of the first non-blank line of `body` — text up to
/// the first `.`, `!` or `?` followed by whitespace or the line end, with a
/// trailing `.` dropped — or that whole line when it has no sentence end. Over
/// `DERIVED_TITLE_MAX_CHARS` chars it is cut on a char boundary and ends in
/// `…`. A blank `body` gives `UNTITLED_FINDING_PLACEHOLDER`. Never empty.
///
/// Fail-open check: this only fills a missing field of a finding inside an
/// otherwise valid review object. It never creates a verdict, never parses
/// prose, and never turns a parse failure into APPROVE; `body` and `verdict`
/// stay required.
/// Test: `parse_long_or_multi_sentence_body_yields_capped_first_sentence`,
/// `parse_untitled_empty_body_gets_placeholder`.
fn derive_title(body: &str) -> String {
    let Some(line) = body.lines().map(str::trim).find(|l| !l.is_empty()) else {
        return UNTITLED_FINDING_PLACEHOLDER.to_string();
    };
    let sentence = first_sentence(line).trim_end_matches('.').trim_end();
    let title = cap_chars(if sentence.is_empty() { line } else { sentence });
    debug_assert!(!title.is_empty() && title.chars().count() <= DERIVED_TITLE_MAX_CHARS);
    title
}

/// `line` up to and including its first sentence end, or all of it.
fn first_sentence(line: &str) -> &str {
    let mut chars = line.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        let at_end = chars.peek().is_none_or(|(_, next)| next.is_whitespace());
        if matches!(c, '.' | '!' | '?') && at_end {
            return &line[..i + c.len_utf8()];
        }
    }
    line
}

/// `text` capped at `DERIVED_TITLE_MAX_CHARS` chars, ellipsis included.
fn cap_chars(text: &str) -> String {
    if text.chars().nth(DERIVED_TITLE_MAX_CHARS).is_none() {
        return text.to_string();
    }
    let cut = text
        .char_indices()
        .nth(DERIVED_TITLE_MAX_CHARS - 1)
        .map_or(text.len(), |(i, _)| i);
    format!("{}…", text[..cut].trim_end())
}

/// Convert an `LlmFinding` wire type to the internal `Finding` type.
///
/// Why: `Finding::new` clamps confidence and normalises effort; the LLM may
/// produce out-of-range values or unknown effort strings.
/// What: maps severity → effort (high/critical → High; medium → Medium; else Low);
/// uses the `title` as the `kind` and `body` as `description`; preserves the
/// finding `category` (#1359 — defaulting to `Correctness` when the model omits
/// it) so the verdict floor can cap a `method-conformance` finding; carries
/// `source_citation` (#1419) when the model provides it.
/// Test: covered transitively by `parse_json_block_happy_path`,
/// `parse_method_conformance_finding_category`, and
/// `parse_finding_carries_source_citation`.
fn convert_llm_finding(f: LlmFinding) -> Finding {
    let effort = match f.severity.to_lowercase().as_str() {
        "high" | "critical" => Effort::High,
        "medium" => Effort::Medium,
        _ => Effort::Low,
    };
    let file = if f.file.is_empty() {
        crate::models::UNKNOWN_FILE_PLACEHOLDER.to_string()
    } else {
        f.file
    };
    let category = f.category;
    let line = f.line;
    // #9310: only a blank title is replaced; a non-empty one passes through as sent.
    let title = if f.title.trim().is_empty() {
        derive_title(&f.body)
    } else {
        f.title
    };
    let mut finding = Finding::new(file, title, f.body, String::new(), f.confidence, effort)
        .with_category(category);
    finding.line = line;
    // Carry the failure consequence through for the inline comment (#1416).
    finding.consequence = f.consequence;
    // Carry the committable replacement code through for a GitHub suggestion
    // block (#1415); normalise empty/whitespace-only strings to None.
    finding.suggested_replacement = f.suggested_replacement.filter(|s| !s.trim().is_empty());
    // Carry the spec/ticket source citation through (#1419); normalise
    // empty/whitespace-only strings to None.
    finding.source_citation = f.source_citation.filter(|s| !s.trim().is_empty());
    // Carry the core-algorithmic-correctness flag through (#PR84); the verdict
    // floor uses it (OR a source_citation) to decide whether a High finding may
    // drive the BLOCK floor.
    finding.code_provable = f.code_provable;
    finding
}

// ─── Strategy 3: Verdict keyword scan ────────────────────────────────────────

/// Scan the last 20% of the body for a verdict keyword (spec REV-112).
///
/// Why: when the LLM ignores the JSON output format, the verdict is often still
/// present as a plain token at or near the end of the response.
/// What: searches the last 20% of `body` (minimum 200 chars) for the verdict
/// tokens in priority order (BLOCK > REQUEST_CHANGES > APPROVE* > APPROVE > UNKNOWN).
/// Returns `None` if no token is found.
/// Test: `parse_verdict_keyword_fallback`, `scan_verdict_keyword_detects_unknown`.
fn scan_verdict_keyword(body: &str) -> Option<Verdict> {
    let scan_start = body.len().saturating_sub((body.len() / 5).max(200));
    let tail = &body[scan_start..];

    // Priority order: most severe first so "BLOCK" beats "APPROVE" if both appear.
    // APPROVE* must be checked before APPROVE so the star variant wins.
    if tail.contains("BLOCK") {
        return Some(Verdict::Block);
    }
    if tail.contains("REQUEST_CHANGES") {
        return Some(Verdict::RequestChanges);
    }
    if tail.contains("APPROVE*") {
        return Some(Verdict::ApproveWithReservations);
    }
    if tail.contains("APPROVE") {
        return Some(Verdict::Approve);
    }
    if tail.contains("UNKNOWN") {
        return Some(Verdict::Unknown);
    }
    None
}

// ─── Malformed json fence (#9310) ─────────────────────────────────────────────

/// Whether `body` holds a `json`/`jsonc` fence whose body is not a valid
/// review object.
///
/// Why: when the model's own fenced object is broken, the fail-safe reason
/// should say so; a reply like that is never parsed from anywhere else (#9310).
/// What: line-based. A line whose trimmed text starts with three backticks
/// opens a fence, and a later bare backtick line closes it; an unclosed fence
/// runs to the end. A fence tagged `json` or `jsonc` (any case) is malformed
/// when its trimmed body does not deserialise as `LlmOutputBlock`. Only the
/// fail-safe reason depends on this; the verdict is UNKNOWN either way.
/// Test: `parse_malformed_json_fence_is_named_and_unknown`,
/// `parse_valid_uppercase_json_fence_is_not_called_malformed`.
fn has_malformed_json_fence(body: &str) -> bool {
    let malformed = |text: &str| serde_json::from_str::<LlmOutputBlock>(text.trim()).is_err();
    // `Some(is_json)` while inside a fence, with the byte offset its body starts at.
    let mut open: Option<(bool, usize)> = None;
    let mut offset = 0;
    for line in body.split_inclusive('\n') {
        let line_end = offset + line.len();
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            let info = trimmed.trim_start_matches('`').trim();
            match open {
                None => {
                    let tag = info.split_whitespace().next().unwrap_or("");
                    let is_json =
                        tag.eq_ignore_ascii_case("json") || tag.eq_ignore_ascii_case("jsonc");
                    open = Some((is_json, line_end));
                }
                Some((is_json, start)) if info.is_empty() => {
                    if is_json && malformed(&body[start..offset]) {
                        return true;
                    }
                    open = None;
                }
                Some(_) => {}
            }
        }
        offset = line_end;
    }
    matches!(open, Some((true, start)) if malformed(&body[start..]))
}

// ─── Grade field extraction ───────────────────────────────────────────────────

/// Extract and validate the grade field from the LLM output block.
///
/// Why: the LLM may omit the grade, emit an empty string, or produce an
/// invalid value.  The pipeline must degrade gracefully — an unparseable grade
/// never panics; it returns `None` and the runner falls back to
/// `default_grade_for_verdict`.
/// What: trims whitespace; if empty → `None`; validates against the 13 known
/// grade strings ("A+", "A", … "F"); invalid strings produce a warning and
/// return `None`.
/// Test: covered transitively by `parse_direct_json_with_grade`.
fn extract_grade_field(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Validate against the 13 canonical grade strings.
    const VALID_GRADES: &[&str] = &[
        "A+", "A", "A-", "B+", "B", "B-", "C+", "C", "C-", "D+", "D", "D-", "F",
    ];
    if VALID_GRADES.contains(&trimmed) {
        Some(trimmed.to_string())
    } else {
        warn!(
            grade = trimmed,
            "LLM returned unrecognised grade — ignoring (will use default)"
        );
        None
    }
}

// ─── Verdict string normalization ─────────────────────────────────────────────

/// Parse a verdict string from the JSON block into a `Verdict`.
///
/// Why: the LLM may emit slightly varied case or include extra whitespace.
/// What: normalises to uppercase and matches against the five board grade
/// tokens; returns `None` for unrecognised strings (caller applies fail-safe).
/// Test: `parse_verdict_string_normalization`.
fn parse_verdict_string(s: &str) -> Option<Verdict> {
    match s.trim().to_uppercase().as_str() {
        "APPROVE" => Some(Verdict::Approve),
        "APPROVE*" => Some(Verdict::ApproveWithReservations),
        "REQUEST_CHANGES" | "REQUEST CHANGES" => Some(Verdict::RequestChanges),
        "BLOCK" => Some(Verdict::Block),
        "UNKNOWN" => Some(Verdict::Unknown),
        _ => None,
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

// ─── Unit tests ─────────────────────────────────────────────────────────────
// Tests extracted to parser_tests.rs to keep this file under the 500-line cap.

#[cfg(test)]
#[path = "parser_tests.rs"]
mod tests;
