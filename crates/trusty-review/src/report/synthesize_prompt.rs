//! Prompt + JSON contract builder for report synthesis (M2, #2314).
//!
//! Why: the synthesizer sends the deterministic report data to the LLM and must
//! (a) forbid invention of any figure, (b) enforce the no-green-analysis rule
//! STRUCTURALLY — green findings are never placed in the prompt, so the model
//! cannot elaborate them even if instructed to — and (c) force a structured JSON
//! response so parsing never depends on free-text scraping.
//! What: [`build_synthesis_prompt`] assembles the [`LlmRequest`] (system prompt +
//! data digest + [`ResponseSchema`]); the digest lists per-application profile
//! numbers and ONLY the RED/AMBER findings.  The `bedrock/`/`openrouter/` routing
//! prefix is stripped so the bare id reaches the provider API.
//! Test: `synthesize_tests.rs` asserts schema shape, that greens are absent from
//! the digest, and that the prefix is stripped.

use crate::llm::{ChatMessage, LlmRequest, ResponseSchema, strip_provider_prefix};
use crate::report::metrics::Severity;
use crate::report::model::ReportModel;

/// Temperature for synthesis — low, to keep the analytic voice grounded.
pub(super) const SYNTHESIS_TEMPERATURE: f32 = 0.2;
/// Output-token ceiling for one synthesis pass.
pub(super) const SYNTHESIS_MAX_TOKENS: u32 = 3072;

/// The JSON Schema the provider forces the model to emit.
///
/// Why: forced structured output removes the "did the model wrap it in a fence?"
/// class of parse failure; the response body IS the JSON object.
/// What: returns a [`ResponseSchema`] named `report_synthesis` with the exec
/// summary, a top-risks array, and a RED/AMBER findings array.  Every finding
/// carries the `app_slug` and `severity` so the reporter can route the prose
/// back to the correct application section and band.
/// Test: `synthesize_tests.rs::synthesis_schema_shape`.
pub(super) fn synthesis_schema() -> ResponseSchema {
    ResponseSchema {
        name: "report_synthesis".to_string(),
        schema: serde_json::json!({
            "type": "object",
            "properties": {
                "executive_summary": {
                    "type": "string",
                    "description": "One deal-relevant paragraph synthesised across all applications. Use ONLY figures present in the provided data; never invent numbers; preserve any \"(approx)\" marker verbatim."
                },
                "top_risks": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "description": {"type": "string"},
                            "severity": {"type": "string", "enum": ["RED", "AMBER"]},
                            "cost": {"type": "string", "description": "Qualitative effort/cost framing; no invented figures."},
                            "apps": {"type": "string", "description": "Affected application name(s)."}
                        },
                        "required": ["description", "severity", "cost", "apps"]
                    },
                    "description": "3-5 top risks, most material first. Drawn ONLY from the RED/AMBER findings provided."
                },
                "findings": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "app_slug": {"type": "string", "description": "Must match a provided application slug exactly."},
                            "title": {"type": "string"},
                            "severity": {"type": "string", "enum": ["RED", "AMBER"]},
                            "description": {"type": "string"},
                            "evidence": {"type": "string"},
                            "component": {"type": "string"},
                            "business_impact": {"type": "string"},
                            "remediation": {"type": "string"},
                            "cost_effort": {"type": "string"}
                        },
                        "required": ["app_slug", "title", "severity", "description", "remediation"]
                    },
                    "description": "Elaboration prose for EACH provided RED/AMBER finding. Do NOT add findings not in the provided list."
                }
            },
            "required": ["executive_summary", "top_risks", "findings"]
        }),
    }
}

/// The synthesis system prompt.
///
/// Why: the deal-analytic voice + the hard grounding rules (no invented numbers,
/// preserve approx markers, RED/AMBER only) belong in the system role so they
/// govern every field.
/// What: a static instruction block; the no-green rule is stated for defence in
/// depth even though greens are also excluded structurally from the digest.
/// Test: covered transitively by the prompt-assembly tests.
pub(super) fn synthesis_system_prompt() -> &'static str {
    r#"You are a senior technical due-diligence analyst writing the narrative sections of an acquisition-grade report from pre-computed repository analysis data.

## Absolute rules
- Use ONLY values present in the provided data. NEVER invent, estimate, or extrapolate a number that is not given.
- If a provided value carries an "(approx)" marker, preserve that marker verbatim.
- Write prose for RED and AMBER findings ONLY. Do not mention, elaborate, or infer GREEN/positive findings — none are provided, and none should appear.
- Be concise, specific, and deal-relevant: what an acquirer must act on.

## Output
Populate the structured response:
- `executive_summary`: one paragraph synthesised across all applications (not a restatement of section headers).
- `top_risks`: the 3-5 most material RED/AMBER risks, each with a severity, a qualitative cost/effort framing, and the affected application(s).
- `findings`: one elaboration per provided RED/AMBER finding (finding, evidence, affected component, business impact, remediation framing), tagged with its `app_slug` and `severity`."#
}

/// Build the LLM request for one report-synthesis pass.
///
/// Why: single place that turns the deterministic [`ReportModel`] into a grounded
/// prompt; keeping greens out here is what makes the no-green rule structural
/// rather than a polite request.
/// What: assembles the system prompt, a per-application data digest containing
/// only RED/AMBER findings, and the forced-output schema.  Strips any provider
/// routing prefix from `llm_model`.
/// Test: `synthesize_tests.rs::{prompt_excludes_greens, prompt_strips_prefix,
/// synthesis_schema_shape}`.
pub(super) fn build_synthesis_prompt(model: &ReportModel, llm_model: &str) -> LlmRequest {
    LlmRequest {
        model: strip_provider_prefix(llm_model).to_string(),
        system: synthesis_system_prompt().to_string(),
        messages: vec![ChatMessage {
            role: "user".to_string(),
            content: build_digest(model),
        }],
        temperature: SYNTHESIS_TEMPERATURE,
        max_tokens: SYNTHESIS_MAX_TOKENS,
        response_schema: Some(synthesis_schema()),
    }
}

/// Build the user-message data digest — RED/AMBER findings only.
///
/// Why: the model must see every deterministic figure it may cite and every
/// RED/AMBER finding it must elaborate, and NOTHING it must not (greens).
/// What: writes a report header then, per application, its profile numbers and a
/// bulleted list of its non-green findings; greens are filtered out before the
/// list is written.
/// Test: `synthesize_tests.rs::prompt_excludes_greens`.
fn build_digest(model: &ReportModel) -> String {
    let mut msg = String::with_capacity(2048);
    msg.push_str(&format!("# Report: {}\n\n", model.title));
    msg.push_str(&format!(
        "Applications assessed: {}\n\n",
        model
            .repositories
            .iter()
            .map(|r| r.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    ));

    for repo in &model.repositories {
        msg.push_str(&format!("## {} (slug: {})\n", repo.name, repo.slug));
        if let Some(m) = &repo.metrics {
            if m.loc.total > 0 {
                msg.push_str(&format!("- Total LoC: {}\n", m.loc.total));
            }
            let langs = m.primary_languages(4);
            if !langs.is_empty() {
                msg.push_str(&format!("- Primary languages: {}\n", langs.join(", ")));
            }
            msg.push_str(&format!(
                "- Files: {}; Functions: {}\n",
                m.counts.files, m.counts.functions
            ));

            // NO-GREEN RULE (structural): only RED/AMBER findings reach the model.
            let actionable: Vec<_> = m
                .findings
                .iter()
                .filter(|f| f.severity != Severity::Green)
                .collect();
            if actionable.is_empty() {
                msg.push_str("- Findings (RED/AMBER): none\n");
            } else {
                msg.push_str("- Findings (RED/AMBER):\n");
                for f in actionable {
                    let sev = match f.severity {
                        Severity::Red => "RED",
                        Severity::Amber => "AMBER",
                        Severity::Green => unreachable!("greens filtered above"),
                    };
                    msg.push_str(&format!(
                        "  - [{sev}] {} (category: {}, component: {})\n",
                        f.title, f.category, f.component
                    ));
                }
            }
        } else {
            msg.push_str("- No metrics available for this application.\n");
        }
        msg.push('\n');
    }

    msg.push_str(
        "Synthesise the narrative sections from the data above, obeying every rule in the system prompt.\n",
    );
    msg
}
