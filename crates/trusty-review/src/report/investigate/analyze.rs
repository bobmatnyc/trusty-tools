//! LLM request/response for the repo-evidence investigation pass (wave 3, #2357).
//!
//! Why: after deterministic selection picks the evidence-bearing files, the
//! reviewer-role LLM reads them (in bounded batches — see [`super::batch`]) and
//! proposes findings.  This is the ONE place an LLM touches the investigation,
//! and its whole output is untrusted: every finding is re-checked by the
//! deterministic evidence guardrail ([`super::verify`]) before it can appear in
//! the report.  Forcing structured output (a `ResponseSchema`) means parsing
//! never depends on scraping free text.
//!
//! A live-QA incident (batch collapse, #2357 follow-up) found that a single
//! unbatched request on a 175-file repo sent 282 KB of file content, the
//! findings JSON hit the 4096-token output ceiling mid-array, `finish_reason =
//! length` correctly failed closed — but that meant ZERO findings survived even
//! though the repo was fully readable, reproducing the exact "no evidence-based
//! conclusions can be drawn" lament the tool exists to eliminate. The fix is
//! structural, not a bigger constant: bound the INPUT per request (batching,
//! [`super::batch::BATCH_MAX_BYTES`]), bound the OUTPUT per request
//! (`maxItems` and `maxLength` in the schema, plus a raised token ceiling as a
//! second line of defence), and retry once, more concisely, on truncation
//! before failing that one batch closed.
//! What: [`investigation_schema`] is the forced JSON contract, parameterised by
//! `max_findings` so a truncation retry can ask for fewer, terser rows;
//! [`build_request`] assembles the [`LlmRequest`] for ONE batch of files (system
//! rules + a per-batch digest with paths + contents + the DD checklist, batch
//! position, and a concise-retry directive when requested); [`parse_findings`]
//! decodes the response into [`RawFinding`]s (pre-verification).
//! Test: `analyze_tests.rs` asserts schema shape (incl. `maxItems`/`maxLength`),
//! that batch file contents + position reach the prompt, the retry directive,
//! prefix stripping, and lenient parse (bare object + fenced block).

use serde::Deserialize;

use crate::llm::{ChatMessage, LlmRequest, ResponseSchema, strip_provider_prefix};

use super::select::{DIMENSIONS, SelectedFile};

/// Temperature for investigation — low, to keep the analysis grounded.
pub(super) const INVESTIGATION_TEMPERATURE: f32 = 0.2;

/// Output-token ceiling for one investigation batch request.
///
/// Why: the live-QA incident showed 4096 was too tight even for a single
/// *bounded* batch's worst case (8 findings, each with several prose fields).
/// 8192 matches the ceiling `pipeline/prompt.rs::GEMINI_MAX_TOKENS` already uses
/// for a demanding structured-output case elsewhere in this crate, so
/// investigation is aligned with the highest existing precedent rather than
/// inventing a new number. This is a SECOND line of defence — the primary fix
/// is bounding the output shape itself via `maxItems`/`maxLength` below, which
/// makes truncation structurally unlikely rather than merely less likely.
pub(super) const INVESTIGATION_MAX_TOKENS: u32 = 8192;

/// Maximum findings requested per batch (first attempt).
///
/// Why: capping the array length is the structural bound that prevents output
/// truncation — 8 well-evidenced findings, each with several prose fields, fits
/// comfortably under [`INVESTIGATION_MAX_TOKENS`] even at the max per-field
/// length below.
pub const MAX_FINDINGS_PER_BATCH: usize = 8;

/// Maximum findings requested on the one truncation retry (more headroom per
/// finding for the same token budget).
pub const RETRY_MAX_FINDINGS: usize = 4;

/// Maximum characters for one `evidence_quote` (schema hint + prompt directive).
///
/// Why: the guardrail already whitespace-normalises before matching, so a short
/// quote verifies exactly as well as a long one; bounding it caps the single
/// largest per-finding field and is a major contributor to output-size control.
pub const EVIDENCE_QUOTE_MAX_CHARS: usize = 200;

/// A finding exactly as the model emits it, before evidence verification.
///
/// Why: the raw shape mirrors the forced schema; verification then rejects any
/// finding whose `file` is not in the selected set or whose `evidence_quote` does
/// not mechanically match that file — so nothing here is trusted as-is.
/// What: severity is a raw string (`red`/`amber`/`green`); `line` is optional and
/// only advisory (verification recomputes it from the match); the prose fields
/// carry the model's narrative.
/// Test: `analyze_tests::parses_bare_object`.
#[derive(Debug, Clone, Deserialize)]
pub struct RawFinding {
    /// Short finding title.
    #[serde(default)]
    pub title: String,
    /// Raw severity token: `red` / `amber` / `green`.
    #[serde(default)]
    pub severity: String,
    /// DD dimension / category this finding belongs to.
    #[serde(default)]
    pub dimension: String,
    /// Repository-relative file the finding cites (must be in the selected set).
    #[serde(default)]
    pub file: String,
    /// Advisory 1-based line (verification recomputes it from the match).
    #[serde(default)]
    pub line: Option<u64>,
    /// Verbatim snippet from `file` proving the finding (substring-verified).
    #[serde(default)]
    pub evidence_quote: String,
    /// One-line description of the finding.
    #[serde(default)]
    pub description: String,
    /// Business-impact framing.
    #[serde(default)]
    pub business_impact: String,
    /// Remediation framing.
    #[serde(default)]
    pub remediation: String,
    /// Qualitative cost/effort framing.
    #[serde(default)]
    pub cost_effort: String,
}

/// The raw investigation response (a `findings` array).
#[derive(Debug, Clone, Deserialize)]
pub struct RawInvestigation {
    /// The proposed findings, pre-verification.
    #[serde(default)]
    pub findings: Vec<RawFinding>,
}

/// The forced JSON Schema the model must emit for one investigation batch.
///
/// Why: structured output removes the "did it wrap the JSON in a fence?" parse
/// failure class; the response body IS the object.  `max_findings` and
/// `evidence_quote`'s `maxLength` are the structural output-size bound (see the
/// module-level Why) — parameterised so a truncation retry can ask for fewer,
/// terser rows via a tighter schema rather than hoping the model complies with
/// prose alone.
/// What: returns a [`ResponseSchema`] named `repo_investigation` whose `findings`
/// array caps at `max_findings` items, each requiring title/severity/dimension/
/// file/evidence_quote/description/remediation, with optional line/
/// business_impact/cost_effort.
/// Test: `analyze_tests::{schema_shape, schema_shrinks_on_retry}`.
pub fn investigation_schema(max_findings: usize) -> ResponseSchema {
    ResponseSchema {
        name: "repo_investigation".to_string(),
        schema: serde_json::json!({
            "type": "object",
            "properties": {
                "findings": {
                    "type": "array",
                    "maxItems": max_findings,
                    "items": {
                        "type": "object",
                        "properties": {
                            "title": {"type": "string"},
                            "severity": {"type": "string", "enum": ["red", "amber", "green"]},
                            "dimension": {"type": "string", "description": "The DD dimension this finding addresses."},
                            "file": {"type": "string", "description": "A repository-relative path from the PROVIDED files only."},
                            "line": {"type": "integer", "description": "Approximate 1-based line; a best guess is fine."},
                            "evidence_quote": {
                                "type": "string",
                                "maxLength": EVIDENCE_QUOTE_MAX_CHARS,
                                "description": "A VERBATIM snippet copied from the cited file, at most 200 characters. Must appear character-for-character (whitespace may differ). Empty for GREEN findings."
                            },
                            "description": {"type": "string", "description": "One concise sentence."},
                            "business_impact": {"type": "string", "description": "One concise sentence."},
                            "remediation": {"type": "string", "description": "One concise sentence."},
                            "cost_effort": {"type": "string"}
                        },
                        "required": ["title", "severity", "dimension", "file", "evidence_quote", "description", "remediation"]
                    }
                }
            },
            "required": ["findings"]
        }),
    }
}

/// The investigation system prompt — the hard grounding rules.
///
/// Why: the model must cite only provided files, quote evidence verbatim, invent
/// no figures, keep GREEN findings to a bare title (the no-green-analysis rule),
/// and keep every field short enough that a bounded batch cannot overflow the
/// output ceiling; stating these in the system role governs every field.
/// What: a static instruction block; the verbatim-quote rule is emphasised because
/// the deterministic guardrail rejects any finding whose quote does not match.
/// Test: covered transitively by the request-assembly tests.
fn system_prompt() -> &'static str {
    r#"You are a senior technical due-diligence analyst inspecting a codebase directly. You are given a curated set of source files (path + contents) and a checklist of due-diligence dimensions. Produce concrete, evidence-backed findings.

## Absolute rules
- Cite ONLY the files provided below. Never reference a file that is not in the provided set. You may be seeing only ONE BATCH of a larger repository — do not claim a file is "absent from the codebase" just because it is not in this batch.
- For every RED or AMBER finding, `evidence_quote` MUST be a snippet copied VERBATIM from the cited file — character for character (only whitespace may differ), AND AT MOST 200 CHARACTERS. If you cannot quote real code that short, quote the most essential fragment rather than the whole block. Fabricated or paraphrased evidence will be rejected.
- Never invent numbers, percentages, or metrics. State only what the code shows.
- GREEN findings are a bare title only (a healthy topic) — no evidence, description, or remediation. Use them sparingly.
- Set `severity` to `red` (critical), `amber` (material), or `green` (healthy topic).
- Assign each finding a `dimension` from the checklist. Prefer depth over breadth: a few well-evidenced findings beat many shallow ones.
- Keep every prose field (description, business_impact, remediation) to ONE concise sentence. Verbosity risks the response being cut off, which discards every finding in this batch.

## Output
Return the structured `findings` array, prioritising the highest-severity findings first (they must survive if the response is capped). Each RED/AMBER finding: title, severity, dimension, file, an approximate line, the verbatim evidence_quote (≤200 chars), a one-sentence description, business_impact, remediation, and cost_effort."#
}

/// Build the LLM request for ONE batch of a repository's investigation pass.
///
/// Why: bounding both the input (this batch's files only) and the requested
/// output shape (`max_findings` + the schema's per-field length caps) is what
/// makes truncation structurally unlikely — see the module-level Why for the
/// incident this replaces a bigger-constant fix for.
/// What: assembles the system prompt, a user digest (batch position + DD
/// checklist + analyst focus + each batch file as a fenced `path` + content
/// block), the forced schema capped at `max_findings`, and low temperature.
/// When `retry_concise` is true, appends a directive asking for a maximally
/// terse response (used for the one truncation retry).  Strips any provider
/// routing prefix from `llm_model`.
/// Test: `analyze_tests::{request_embeds_files, request_strips_prefix,
/// request_reports_batch_position, retry_directive_appended}`.
#[allow(clippy::too_many_arguments)]
pub fn build_request(
    app_name: &str,
    files: &[SelectedFile],
    batch_index: usize,
    batch_count: usize,
    total_files: usize,
    instructions: Option<&str>,
    llm_model: &str,
    max_findings: usize,
    retry_concise: bool,
) -> LlmRequest {
    LlmRequest {
        model: strip_provider_prefix(llm_model).to_string(),
        system: system_prompt().to_string(),
        messages: vec![ChatMessage {
            role: "user".to_string(),
            content: build_digest(
                app_name,
                files,
                batch_index,
                batch_count,
                total_files,
                instructions,
                max_findings,
                retry_concise,
            ),
        }],
        temperature: INVESTIGATION_TEMPERATURE,
        max_tokens: INVESTIGATION_MAX_TOKENS,
        response_schema: Some(investigation_schema(max_findings)),
    }
}

/// Build the user-message digest: batch position, DD checklist, analyst focus,
/// then this batch's file bodies.
#[allow(clippy::too_many_arguments)]
fn build_digest(
    app_name: &str,
    files: &[SelectedFile],
    batch_index: usize,
    batch_count: usize,
    total_files: usize,
    instructions: Option<&str>,
    max_findings: usize,
    retry_concise: bool,
) -> String {
    let mut msg = String::with_capacity(4096);
    msg.push_str(&format!(
        "# Investigate application: {app_name}\n\nInspect the files below for due-diligence findings.\n\n",
    ));
    if batch_count > 1 {
        msg.push_str(&format!(
            "This is batch {batch_index} of {batch_count} for this repository ({total_files} files tracked in total; other batches carry the rest — do not treat this batch as the whole codebase).\n\n",
        ));
    }

    msg.push_str("## Due-diligence dimensions to assess\n");
    for dim in DIMENSIONS {
        msg.push_str(&format!("- {dim}\n"));
    }
    msg.push('\n');

    if let Some(instr) = instructions {
        msg.push_str("## Analyst focus directives (weight findings toward these — but never invent to satisfy them)\n\n");
        msg.push_str(instr);
        msg.push_str("\n\n");
    }

    let bytes_sent: usize = files.iter().map(|f| f.content.len()).sum();
    msg.push_str(&format!(
        "## Provided files ({} in this batch; {bytes_sent} bytes)\n\n",
        files.len(),
    ));
    for f in files {
        msg.push_str(&format!(
            "### FILE: {}\n\n```\n{}\n```\n\n",
            f.path, f.content
        ));
    }

    msg.push_str(&format!(
        "Return at most {max_findings} findings that cite ONLY these files, quoting evidence verbatim (≤200 chars) and keeping every prose field to one concise sentence. Prioritise the highest-severity findings. Obey every rule in the system prompt.\n",
    ));
    if retry_concise {
        msg.push_str(&format!(
            "\nIMPORTANT: your previous response for this batch was truncated. Return AT MOST {max_findings} findings this time, maximally concise — the response MUST fit; drop lower-severity findings before shortening evidence below 200 chars.\n",
        ));
    }
    msg
}

/// Parse the provider response into [`RawInvestigation`] (pre-verification).
///
/// Why: forced structured output returns a bare JSON object, but a fenced block is
/// accepted defensively (matching the synthesizer) so a non-conforming provider
/// does not silently fail.
/// What: tries a direct object parse, then a ```json fenced block; `None` when
/// neither yields a decodable object.
/// Test: `analyze_tests::{parses_bare_object, parses_fenced_block, rejects_garbage}`.
pub fn parse_findings(text: &str) -> Option<RawInvestigation> {
    let body = text.trim();
    if body.starts_with('{')
        && let Ok(r) = serde_json::from_str::<RawInvestigation>(body)
    {
        return Some(r);
    }
    let fence_start = body.rfind("```json")?;
    let after = &body[fence_start + 7..];
    let fence_end = after.find("```")?;
    serde_json::from_str::<RawInvestigation>(after[..fence_end].trim()).ok()
}

#[cfg(test)]
#[path = "analyze_tests.rs"]
mod tests;
