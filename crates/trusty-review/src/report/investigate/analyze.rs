//! LLM request/response for the repo-evidence investigation pass (wave 3, #2357).
//!
//! Why: after deterministic selection picks the evidence-bearing files, one
//! reviewer-role LLM call reads them and proposes findings.  This is the ONE
//! place an LLM touches the investigation, and its whole output is untrusted:
//! every finding is re-checked by the deterministic evidence guardrail
//! ([`super::verify`]) before it can appear in the report.  Forcing structured
//! output (a `ResponseSchema`) means parsing never depends on scraping free text.
//! What: [`investigation_schema`] is the forced JSON contract; [`build_request`]
//! assembles the [`LlmRequest`] (system rules + a per-file digest with paths +
//! contents + the DD checklist); [`parse_findings`] decodes the response into
//! [`RawFinding`]s (pre-verification).  One request per repository — the file set
//! and the evidence-verification corpus are inherently per-repo.
//! Test: `analyze_tests.rs` asserts schema shape, that file contents reach the
//! prompt, prefix stripping, and lenient parse (bare object + fenced block).

use serde::Deserialize;

use crate::llm::{ChatMessage, LlmRequest, ResponseSchema, strip_provider_prefix};

use super::select::{DIMENSIONS, Selection};

/// Temperature for investigation — low, to keep the analysis grounded.
pub(super) const INVESTIGATION_TEMPERATURE: f32 = 0.2;
/// Output-token ceiling for one investigation pass.
pub(super) const INVESTIGATION_MAX_TOKENS: u32 = 4096;

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

/// The forced JSON Schema the model must emit for one investigation pass.
///
/// Why: structured output removes the "did it wrap the JSON in a fence?" parse
/// failure class; the response body IS the object.  Every finding must carry the
/// `file` and a verbatim `evidence_quote` so the guardrail can verify it.
/// What: returns a [`ResponseSchema`] named `repo_investigation` whose `findings`
/// items require title/severity/dimension/file/evidence_quote/description/
/// remediation, with optional line/business_impact/cost_effort.
/// Test: `analyze_tests::schema_shape`.
pub fn investigation_schema() -> ResponseSchema {
    ResponseSchema {
        name: "repo_investigation".to_string(),
        schema: serde_json::json!({
            "type": "object",
            "properties": {
                "findings": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "title": {"type": "string"},
                            "severity": {"type": "string", "enum": ["red", "amber", "green"]},
                            "dimension": {"type": "string", "description": "The DD dimension this finding addresses."},
                            "file": {"type": "string", "description": "A repository-relative path from the PROVIDED files only."},
                            "line": {"type": "integer", "description": "Approximate 1-based line; a best guess is fine."},
                            "evidence_quote": {"type": "string", "description": "A VERBATIM snippet copied from the cited file. Must appear character-for-character (whitespace may differ). Empty for GREEN findings."},
                            "description": {"type": "string"},
                            "business_impact": {"type": "string"},
                            "remediation": {"type": "string"},
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
/// no figures, and keep GREEN findings to a bare title (the no-green-analysis
/// rule); stating these in the system role governs every field.
/// What: a static instruction block; the verbatim-quote rule is emphasised because
/// the deterministic guardrail rejects any finding whose quote does not match.
/// Test: covered transitively by the request-assembly tests.
fn system_prompt() -> &'static str {
    r#"You are a senior technical due-diligence analyst inspecting a codebase directly. You are given a curated set of source files (path + contents) and a checklist of due-diligence dimensions. Produce concrete, evidence-backed findings.

## Absolute rules
- Cite ONLY the files provided below. Never reference a file that is not in the provided set.
- For every RED or AMBER finding, `evidence_quote` MUST be a snippet copied VERBATIM from the cited file — character for character (only whitespace may differ). If you cannot quote real code, do not raise the finding. Fabricated or paraphrased evidence will be rejected.
- Never invent numbers, percentages, or metrics. State only what the code shows.
- GREEN findings are a bare title only (a healthy topic) — no evidence, description, or remediation. Use them sparingly.
- Set `severity` to `red` (critical), `amber` (material), or `green` (healthy topic).
- Assign each finding a `dimension` from the checklist. Prefer depth over breadth: a few well-evidenced findings beat many shallow ones.

## Output
Return the structured `findings` array. Each RED/AMBER finding: title, severity, dimension, file, an approximate line, the verbatim evidence_quote, a description, business_impact, remediation, and cost_effort."#
}

/// Build the LLM request for one repository's investigation pass.
///
/// Why: the single place that turns the deterministic [`Selection`] into a grounded
/// prompt — every file the model may cite is embedded here, and nothing else is.
/// What: assembles the system prompt, a user digest (DD checklist + analyst focus +
/// each selected file as a fenced `path` + content block), the forced schema, and
/// low temperature.  Strips any provider routing prefix from `llm_model`.
/// Test: `analyze_tests::{request_embeds_files, request_strips_prefix}`.
pub fn build_request(
    app_name: &str,
    selection: &Selection,
    instructions: Option<&str>,
    llm_model: &str,
) -> LlmRequest {
    LlmRequest {
        model: strip_provider_prefix(llm_model).to_string(),
        system: system_prompt().to_string(),
        messages: vec![ChatMessage {
            role: "user".to_string(),
            content: build_digest(app_name, selection, instructions),
        }],
        temperature: INVESTIGATION_TEMPERATURE,
        max_tokens: INVESTIGATION_MAX_TOKENS,
        response_schema: Some(investigation_schema()),
    }
}

/// Build the user-message digest: DD checklist, analyst focus, then file bodies.
fn build_digest(app_name: &str, selection: &Selection, instructions: Option<&str>) -> String {
    let mut msg = String::with_capacity(4096);
    msg.push_str(&format!(
        "# Investigate application: {app_name}\n\nInspect the files below for due-diligence findings.\n\n",
    ));

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

    msg.push_str(&format!(
        "## Provided files ({} of {} tracked; {} bytes)\n\n",
        selection.files.len(),
        selection.total_files,
        selection.bytes_sent,
    ));
    for f in &selection.files {
        msg.push_str(&format!(
            "### FILE: {}\n\n```\n{}\n```\n\n",
            f.path, f.content
        ));
    }

    msg.push_str(
        "Return findings that cite ONLY these files, quoting evidence verbatim. Obey every rule in the system prompt.\n",
    );
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
