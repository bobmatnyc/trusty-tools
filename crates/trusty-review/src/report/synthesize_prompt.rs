//! Prompt + JSON contract builder for report synthesis (M2, #2314; compact
//! digest + layered section instructions, #2357 follow-up).
//!
//! Why: the synthesizer sends the deterministic report data to the LLM and must
//! (a) forbid invention of any figure, (b) enforce the no-green-analysis rule
//! STRUCTURALLY — green findings are never placed in the prompt, so the model
//! cannot elaborate them even if instructed to — and (c) force a structured JSON
//! response so parsing never depends on free-text scraping.  A live-QA
//! acceptance run then found that once wave-3 investigation verified dozens of
//! real findings, this SAME call — asking for a full prose elaboration of every
//! one of them alongside the executive summary — hit its own output-token
//! ceiling and failed closed, leaving the exec summary and top risks blank atop
//! an otherwise-strong findings section.  The fix (mirroring wave-3's batching
//! fix): bound the INPUT to a compact per-finding digest
//! ([`super::synthesize_digest`]) and bound the OUTPUT by structurally
//! excluding already-verified findings from the elaboration ask entirely (their
//! prose is already authoritative and merged in later regardless).
//! What: [`build_synthesis_prompt`] assembles the [`LlmRequest`] (a system
//! prompt built from the resolved, layered section instructions — see
//! [`super::section_instructions`] — plus the compact data digest and the
//! forced [`ResponseSchema`]).  #6093: the active [`SynthesisTier`] decides
//! both array caps AND the request's `max_tokens` — a rung of the retry ladder
//! raises the budget while shrinking the ask, and the reviewer role's own
//! configured ceiling reaches the request instead of a constant overriding it.
//! The `bedrock/`/`openrouter/`
//! routing prefix is stripped so the bare id reaches the provider API.  #6009
//! shape 3: [`schema_contract_statement`] renders the required field names
//! and a canonical example object directly from the same [`ResponseSchema`]
//! value forced via `response_format`, and embeds that text in the system
//! prompt itself — `response_format` is best-effort for Anthropic-family
//! models (no native strict-JSON mode; OpenRouter forwards but cannot
//! enforce), so the field names are also stated where a model that ignores
//! the schema entirely will still read them.
//! Test: `synthesize_tests.rs` asserts schema shape, that greens are absent from
//! the digest, that the prefix is stripped, the compact-digest cap/overflow
//! behaviour, the layered-instruction override precedence, and that the
//! derived contract statement names every field and reaches the system
//! prompt.

use crate::llm::{ChatMessage, LlmRequest, ResponseSchema, strip_provider_prefix};
use crate::report::model::ReportModel;
use crate::report::section_instructions::{
    self, AUTHORSHIP_SUMMARY, CODE_QUALITY_SUMMARY, EXECUTIVE_SUMMARY, FINDING_ELABORATION,
    SECURITY_SUMMARY, TOP_RISKS,
};
use crate::report::synthesize_digest::{
    build_split_with_cap, gather_compact_findings, render_context_section,
    render_elaboration_section,
};

/// Temperature for synthesis — low, to keep the analytic voice grounded.
pub(super) const SYNTHESIS_TEMPERATURE: f32 = 0.2;

/// Output-token floor for one synthesis pass, below which no configured
/// ceiling may take the request.
///
/// Why: this was the whole ceiling until #6093 — a hardcoded 3072 that ignored
/// `[models.reviewer].max_tokens` entirely. It survives as a FLOOR because a
/// smaller budget cannot fit four narrative paragraphs plus a top-risks table
/// on any model, so honouring a tiny configured value literally would only
/// produce a truncation the operator cannot diagnose.
pub(super) const SYNTHESIS_MIN_MAX_TOKENS: u32 = 3072;

/// Output-token budget used when no role ceiling is plumbed in (tests, and any
/// caller that constructs a [`super::synthesize::Synthesizer`] directly).
///
/// Matches the reviewer role's own `max_tokens` default in
/// `config::role_models`, so an unconfigured run and a default-configured run
/// ask for the same budget.
pub(super) const SYNTHESIS_DEFAULT_MAX_TOKENS: u32 = 4096;

/// Hard ceiling the escalating retry rungs may reach.
///
/// Why: #6093 closure condition 3 — the truncation retry must escalate the
/// ceiling, not only shrink the content. It still needs a bound, because a
/// provider bills every token an escalation buys.
pub(super) const SYNTHESIS_ESCALATED_MAX_TOKENS: u32 = 16384;

/// One rung of the synthesis retry ladder: how much output budget the request
/// asks for, and how large an ask it spends that budget on.
///
/// Why: a 45-finding investigation truncated twice against the pre-#6093 fixed
/// 3072-token ceiling, and the single "concise" retry shrank only `top_risks`
/// (5 → 3) while leaving the ten per-finding elaborations — nine prose fields
/// each, and by far the dominant output cost — untouched at the same ceiling.
/// So the retry could not converge, and an ~8-minute investigation exited 1
/// with no report. Each rung here both RAISES the budget and SHRINKS the ask,
/// so the ladder converges by construction: the last rung asks only for the
/// four narrative paragraphs and three risk rows, at four times the configured
/// budget.
/// What: [`Self::top_risks_cap`] and [`Self::elaboration_cap`] bound the two
/// arrays (schema `maxItems` AND the digest's own target list, so the two can
/// never disagree); [`Self::budget`] scales the caller's configured ceiling.
/// Dropping elaboration on the last rung drops no FINDING — every RED/AMBER
/// finding still renders from the deterministic composition (#5374) and from
/// the investigation's own verified prose, which wins over synthesis prose
/// regardless (see `cli_report::merge_investigation_prose`).
/// Test: `synthesize_tests.rs::{tier_ladder_raises_budget_and_shrinks_ask,
/// narrative_only_tier_omits_findings_from_schema,
/// synthesize_ladder_recovers_a_large_finding_set}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SynthesisTier {
    /// First attempt: the full ask at the configured budget.
    Full,
    /// Second attempt: fewer risk rows, fewer elaborations, double the budget.
    Concise,
    /// Final attempt: narrative + risk rows only, quadruple the budget.
    NarrativeOnly,
}

/// The rungs tried in order, cheapest ask first.
pub(super) const SYNTHESIS_TIER_LADDER: [SynthesisTier; 3] = [
    SynthesisTier::Full,
    SynthesisTier::Concise,
    SynthesisTier::NarrativeOnly,
];

impl SynthesisTier {
    /// `top_risks` array cap for this rung.
    pub(crate) fn top_risks_cap(self) -> usize {
        match self {
            SynthesisTier::Full => 5,
            SynthesisTier::Concise | SynthesisTier::NarrativeOnly => 3,
        }
    }

    /// `findings` (elaboration) array cap for this rung; 0 omits the array
    /// from both the schema and the digest.
    pub(crate) fn elaboration_cap(self) -> usize {
        match self {
            SynthesisTier::Full => super::synthesize_digest::ELABORATION_TARGETS_CAP,
            SynthesisTier::Concise => 3,
            SynthesisTier::NarrativeOnly => 0,
        }
    }

    /// The request's `max_tokens` for this rung, from the caller's configured
    /// ceiling, clamped into
    /// `[SYNTHESIS_MIN_MAX_TOKENS, SYNTHESIS_ESCALATED_MAX_TOKENS]`.
    pub(crate) fn budget(self, configured: u32) -> u32 {
        let scaled = match self {
            SynthesisTier::Full => configured,
            SynthesisTier::Concise => configured.saturating_mul(2),
            SynthesisTier::NarrativeOnly => configured.saturating_mul(4),
        };
        scaled.clamp(SYNTHESIS_MIN_MAX_TOKENS, SYNTHESIS_ESCALATED_MAX_TOKENS)
    }

    /// True for every rung after the first — the response was truncated, so the
    /// prompt says so and asks for a shorter answer.
    fn is_retry(self) -> bool {
        self != SynthesisTier::Full
    }
}

/// The JSON Schema the provider forces the model to emit.
///
/// Why: forced structured output removes the "did the model wrap it in a fence?"
/// class of parse failure; the response body IS the JSON object.  `top_risks`
/// and `findings` both carry a `maxItems` bound (#2357 follow-up) — a
/// STRUCTURAL cap, not merely a polite request, mirroring the wave-3 batch
/// investigation's `maxItems`/`maxLength` fix for the identical class of
/// output-truncation failure.
/// What: returns a [`ResponseSchema`] named `report_synthesis`; `top_risks_cap`
/// and `findings_cap` bound the two arrays at whatever the active
/// [`SynthesisTier`] asks for, so the schema structurally cannot exceed what
/// the digest lists. `findings_cap == 0` removes the `findings` property
/// altogether (#6093's final rung), which also removes it from
/// [`schema_contract_statement`] — the model is then never invited to spend
/// output budget on prose the report body already carries.
/// Test: `synthesize_tests.rs::{synthesis_schema_shape, schema_shrinks_on_retry,
/// narrative_only_tier_omits_findings_from_schema}`.
///
/// #5675: built via [`ResponseSchema::new`], which applies `enforce_strict_mode`.
/// This schema previously used a struct literal and so never got that pass —
/// `findings.items` carried no `additionalProperties`, and OpenAI strict mode
/// (forwarded by OpenRouter for `openai/*`) rejected every synthesis call.
pub(crate) fn synthesis_schema(top_risks_cap: usize, findings_cap: usize) -> ResponseSchema {
    let mut schema = serde_json::json!({
            "type": "object",
            "properties": {
                // #6030: the summary opens with what the system is and does and
                // names its major components, then covers risk — see the
                // "What the codebase does" block in `synthesis_system_prompt`.
                "executive_summary": {
                    "type": "string",
                    "description": "A short deal-relevant executive summary synthesised across all applications: open by stating what the audited codebase IS and DOES, name its major components and each one's role, then cover the risk picture. Use ONLY figures present in the provided data; never invent numbers; preserve any \"(approx)\" marker verbatim."
                },
                // #6004: two additional narrative slots, same schema/call/guardrail as
                // executive_summary — see synthesize.rs::apply_guardrail.
                "code_quality_summary": {
                    "type": "string",
                    "description": "ONE paragraph on code quality and architecture, grounded ONLY in the complexity/findings/LoC data provided. Empty string if the data supports nothing."
                },
                "security_summary": {
                    "type": "string",
                    "description": "ONE paragraph characterising security posture from the evidence-inspected RED/AMBER findings ONLY — this is code-hygiene signal, not lint output and not a SAST/CVE/secrets scan. Empty string if the data supports nothing."
                },
                // #5453/#6004: high-level authorship/key-person-risk narrative slot.
                "authorship_summary": {
                    "type": "string",
                    "description": "ONE high-level paragraph on codebase health from an authorship perspective — trailing-12-month trajectory and key-person risk, grounded ONLY in the bus-factor/concentration/trajectory data provided. Not a data dump. Empty string if the data supports nothing."
                },
                "top_risks": {
                    "type": "array",
                    "maxItems": top_risks_cap,
                    "items": {
                        "type": "object",
                        "properties": {
                            "description": {"type": "string"},
                            "severity": {"type": "string", "enum": ["RED", "AMBER"]},
                            "cost": {"type": "string", "description": "Qualitative effort/cost framing; no invented figures."},
                            "apps": {"type": "string", "description": "Affected application name(s)."}
                        }
                    },
                    "description": "The most material RED/AMBER risks, most material first, drawn ONLY from the findings provided."
                },
                "findings": {
                    "type": "array",
                    "maxItems": findings_cap,
                    "items": {
                        "type": "object",
                        "properties": {
                            "app_slug": {"type": "string", "description": "Must match a provided application slug exactly."},
                            "title": {"type": "string"},
                            "severity": {"type": "string", "enum": ["RED", "AMBER"]},
                            "description": {"type": "string"},
                            // #5675: strict mode requires every property in
                            // `required`, so these four can no longer be omitted.
                            // Say empty is acceptable, or the model invents filler
                            // to fill a key it has nothing to put in.
                            "evidence": {"type": "string", "description": "Supporting evidence drawn from the provided data. Empty string if the data provides none."},
                            "component": {"type": "string", "description": "Affected component or path. Empty string if the data does not identify one."},
                            "business_impact": {"type": "string", "description": "Business-impact framing. Empty string if the data does not support one — never invent an impact to fill this field."},
                            "remediation": {"type": "string"},
                            "cost_effort": {"type": "string", "description": "Qualitative effort/cost framing; no invented figures. Empty string if unknown."}
                        }
                    },
                    "description": "Elaboration prose EXCLUSIVELY for the findings listed under \"Findings requiring elaboration\". Leave empty when that list says none. Never add a finding not in that list, and never re-elaborate a finding tagged [verified] elsewhere in the data."
                }
            }
    });

    if findings_cap == 0
        && let Some(props) = schema["properties"].as_object_mut()
    {
        props.remove("findings");
    }
    ResponseSchema::new("report_synthesis", schema)
}

/// Render a deterministic statement of the required JSON shape directly from
/// the schema's own field names, for embedding in the prompt TEXT itself
/// (#6009 shape 3).
///
/// Why: `response_format: json_schema` (even `strict: true`) is best-effort
/// for Anthropic-family models routed through OpenRouter — Anthropic's API
/// has no native strict-JSON mode, so a live 3/3 repro against
/// `anthropic/claude-opus-4.8` produced three different response shapes on
/// three consecutive calls with the identical `response_schema` attached: pure
/// markdown (no JSON at all), then two rounds of `top_risks` field-name
/// drift. `response_format` alone cannot be trusted to reach a model that
/// ignores it; stating the exact contract in the prompt's own text is a
/// second, independent path to the same field names — and generating that
/// text FROM `schema` (rather than a hand-typed string beside it) is what
/// keeps the two from silently drifting apart, since [`synthesis_schema`] is
/// the only place either originates.
/// What: walks `schema`'s top-level `properties`; for a scalar property
/// (`executive_summary`) states its name, for an array-of-objects property
/// (`top_risks`, `findings`) lists its item field names, then appends one
/// compact canonical-shape example object built the same way. Property
/// iteration order is [`serde_json::Map`]'s default (`BTreeMap`, this
/// workspace does not enable `preserve_order`) — alphabetical and stable run
/// to run.
/// Test: `synthesize_tests.rs::{schema_contract_statement_lists_every_field_name,
/// schema_contract_statement_reaches_system_prompt}`.
pub(crate) fn schema_contract_statement(schema: &serde_json::Value) -> String {
    let Some(props) = schema["properties"].as_object() else {
        return String::new();
    };
    let mut lines = String::from("Required JSON object shape — use EXACTLY these field names:\n");
    let mut example = serde_json::Map::new();

    for (name, def) in props {
        match def["items"]["properties"].as_object() {
            Some(item_props) => {
                let field_names: Vec<&str> = item_props.keys().map(String::as_str).collect();
                lines.push_str(&format!(
                    "- `{name}`: array of objects, each with fields: {}\n",
                    field_names.join(", ")
                ));
                let mut item_example = serde_json::Map::new();
                for f in &field_names {
                    item_example.insert(
                        (*f).to_string(),
                        serde_json::Value::String(format!("<{f}>")),
                    );
                }
                example.insert(
                    name.clone(),
                    serde_json::Value::Array(vec![serde_json::Value::Object(item_example)]),
                );
            }
            None => {
                lines.push_str(&format!("- `{name}`: string\n"));
                example.insert(name.clone(), serde_json::Value::String(format!("<{name}>")));
            }
        }
    }

    lines.push_str(&format!(
        "\nExample shape (field names only — write your own content, never copy these placeholder values):\n{}\n",
        serde_json::to_string(&serde_json::Value::Object(example)).unwrap_or_default()
    ));
    lines
}

/// The synthesis system prompt, built from the resolved (layered) section
/// instructions.
///
/// Why: the hard grounding rules (no invented numbers, preserve approx
/// markers, RED/AMBER only) are the tool's OWN invariants and never vary; the
/// per-section "what to write" guidance, by contrast, is exactly the layer a
/// template or analyst may steer — see [`section_instructions`] for the
/// generic → template → analyst layering (the same shape as the crate's
/// existing stock → principles → voice reviewer layering, see `src/voice/`).
/// What: the "Absolute rules", "Required JSON object shape", "What the
/// codebase does", and "Coverage gaps in the assessed set" blocks are static
/// invariants; the "Required JSON
/// object shape" block is `contract` — [`schema_contract_statement`] run over
/// the SAME [`synthesis_schema`] value this request forces via
/// `response_format`, so the two can never say different things (#6009 shape
/// 3). The "Output" section embeds `resolved[EXECUTIVE_SUMMARY]` /
/// `resolved[TOP_RISKS]` / `resolved[FINDING_ELABORATION]` verbatim.
/// `retry_concise` appends a directive to shorten the response (used on the
/// one truncation retry).
///
/// The coverage-gaps block is static rather than a fourth section instruction
/// because a template that overrides `executive_summary` would otherwise drop
/// it, and because the response schema is fixed — the executive summary is the
/// only narrative field the model controls, so the note has nowhere else to
/// go.  It is scoped to targets ABSENT from the assessed set, which is a
/// different question from the per-repo dimension coverage that
/// [`super::investigate::render::coverage_prompt_summary`] already mandates.
///
/// #6030's "What the codebase does" block is static for the same reason: the
/// owner requires every executive summary to describe the product and analyze
/// its major components, so a template `instruct:` override must not be able
/// to drop that requirement while restyling the section.
/// Test: `synthesize_tests.rs::{system_prompt_embeds_resolved_instructions,
/// system_prompt_concise_retry_directive,
/// system_prompt_asks_for_unregistered_target_gaps,
/// system_prompt_asks_what_the_codebase_does,
/// schema_contract_statement_reaches_system_prompt}`.
fn synthesis_system_prompt(
    resolved: &std::collections::BTreeMap<String, String>,
    retry_concise: bool,
    contract: &str,
) -> String {
    let exec = resolved
        .get(EXECUTIVE_SUMMARY)
        .map(String::as_str)
        .unwrap_or_default();
    let risks = resolved
        .get(TOP_RISKS)
        .map(String::as_str)
        .unwrap_or_default();
    let elaboration = resolved
        .get(FINDING_ELABORATION)
        .map(String::as_str)
        .unwrap_or_default();
    // #6004: two additional resolved narrative slots.
    let code_quality = resolved
        .get(CODE_QUALITY_SUMMARY)
        .map(String::as_str)
        .unwrap_or_default();
    let security = resolved
        .get(SECURITY_SUMMARY)
        .map(String::as_str)
        .unwrap_or_default();
    // #5453/#6004: the high-level authorship/key-person-risk narrative slot.
    let authorship = resolved
        .get(AUTHORSHIP_SUMMARY)
        .map(String::as_str)
        .unwrap_or_default();

    let mut prompt = format!(
        r#"You are a senior technical due-diligence analyst writing the narrative sections of an acquisition-grade report from pre-computed repository analysis data.

## Voice
Write from the acquirer's side: balanced but adversarial. Hunt for risk skeptically — never read a finding as more benign than the data supports — while still naming genuine strengths the data actually shows, evenhandedly. Never write promotional or vendor-style prose.

## Absolute rules
- Use ONLY values present in the provided data. NEVER invent, estimate, or extrapolate a number that is not given.
- If a provided value carries an "(approx)" marker, preserve that marker verbatim.
- Write prose for RED and AMBER findings ONLY. Do not mention, elaborate, or infer GREEN/positive findings — none are provided, and none should appear.
- Be concise, specific, and deal-relevant: what an acquirer must act on.
- Every finding field is structurally required. Emit an empty string for any you have nothing grounded to say in — never fill one with invented content.

## Required JSON object shape
Respond with ONLY a single JSON object — no markdown headings, no prose outside it, no fenced code block. {contract}

## What the codebase does
- OPEN the executive summary by stating what the audited system IS and what it DOES — its purpose in plain product terms — before any risk sentence. A risk-only summary is incomplete.
- Then analyze the major components: name each one and what its role in the system is. Draw them from the applications listed above and from the build manifests, frameworks, and declared dependencies the data names for each.
- Ground both from the provided data ONLY — application names, detected manifests and frameworks, language mix, LoC, authorship figures. Where the data does not support a claim about purpose, describe what the components do and stop. Never invent a product, a market, a customer, or a component the data does not name.
- Risk remains the summary's core. The purpose and component narrative sets up the risk discussion; it does not replace it.

## Coverage gaps in the assessed set
- The applications listed under "Applications assessed" are the ENTIRE assessed set. When the data references a repository, service, database schema, or project board that is not one of them, name it in a short coverage-gaps note closing the executive summary.
- In that note, recommend the operator register the named targets and re-run, so the report reflects the full technology estate under assessment.
- Check specifically for schema and migration repositories: a finding citing a table, a migration, or a stored procedure with no corresponding schema repository in the assessed set is exactly this case, and it is the omission operators make most often.
- Name ONLY targets the data above actually references. Never list one you infer probably exists, and write no note at all when the data references nothing outside the assessed set.

## Output
Populate the structured response:
- `executive_summary`: {exec}
- `top_risks`: {risks}
- `findings`: {elaboration}
- `code_quality_summary`: {code_quality}
- `security_summary`: {security}
- `authorship_summary`: {authorship}"#
    );

    if retry_concise {
        prompt.push_str(
            "\n\n## Retry directive\nYour previous response was truncated. This time, be maximally concise: a shorter executive-summary paragraph and fewer top-risk rows. The response MUST fit.",
        );
    }
    prompt
}

/// Build the LLM request for one report-synthesis pass.
///
/// Why: single place that turns the deterministic [`ReportModel`] into a
/// grounded, size-bounded prompt; keeping greens out here is what makes the
/// no-green rule structural rather than a polite request, and building the
/// digest from [`super::synthesize_digest`] is what keeps input/output
/// bounded regardless of how many findings a large repository's investigation
/// verifies.
/// What: resolves the active section instructions (template override else
/// generic default, via [`section_instructions::resolve`]), assembles the
/// dynamic system prompt, a compact data digest, and the size-bounded
/// forced-output schema.  `tier` decides both array caps and the request's
/// `max_tokens` (see [`SynthesisTier`]); `configured_max_tokens` is the
/// reviewer role's own ceiling, which reaches the request here rather than
/// being overridden by a constant (#6093).  Strips any provider routing prefix
/// from `llm_model`.
/// Test: `synthesize_tests.rs::{prompt_excludes_greens, prompt_strips_prefix,
/// synthesis_schema_shape, digest_uses_compact_findings,
/// template_override_reaches_system_prompt,
/// tier_ladder_raises_budget_and_shrinks_ask}`.
pub(super) fn build_synthesis_prompt(
    model: &ReportModel,
    llm_model: &str,
    tier: SynthesisTier,
    configured_max_tokens: u32,
) -> LlmRequest {
    let resolved = section_instructions::resolve(&model.section_instructions);
    // #6009 shape 3: built from the SAME schema value forced via
    // `response_format` below, so the prompt-text contract and the schema can
    // never say different things — see `schema_contract_statement`.
    let schema = synthesis_schema(tier.top_risks_cap(), tier.elaboration_cap());
    let contract = schema_contract_statement(&schema.schema);
    LlmRequest {
        model: strip_provider_prefix(llm_model).to_string(),
        system: synthesis_system_prompt(&resolved, tier.is_retry(), &contract),
        messages: vec![ChatMessage {
            role: "user".to_string(),
            content: build_digest(model, tier.elaboration_cap()),
        }],
        temperature: SYNTHESIS_TEMPERATURE,
        max_tokens: tier.budget(configured_max_tokens),
        response_schema: Some(schema),
    }
}

/// One application's profile bullets for the digest — density plus the
/// component grounding the executive summary is required to analyze (#6030).
///
/// Why: the "What the codebase does" instruction can only be obeyed from data
/// in the prompt, and the build manifests, declared project names, and top
/// dependencies the repository scan already detects ARE the component
/// evidence. They reached the rendered Profile table
/// (`reporter_fill::fill_profile`) but never the synthesis prompt.
/// What: LoC and languages from `AnalyzeMetrics` when a `--analyze` run
/// supplied it, else from `RepoScan`; file/function counts from whichever
/// source carries them; and the scan's framework line rendered through
/// `reporter_fill::format_frameworks`, so the prompt and the report state the
/// same manifests. Returns an empty string when neither source has anything —
/// the caller then says so rather than emitting a headed but empty block.
/// Test: `synthesize_tests.rs::{digest_carries_scan_profile_without_metrics,
/// digest_names_build_manifests_as_component_evidence}`.
fn repo_profile_lines(repo: &super::model::RepositoryReport) -> String {
    let metrics = repo.metrics.as_ref();
    let scan = repo.scan.as_ref();
    let mut out = String::new();

    let loc = metrics
        .map(|m| m.loc.total)
        .filter(|n| *n > 0)
        .or_else(|| scan.map(|s| s.total_loc).filter(|n| *n > 0));
    if let Some(n) = loc {
        out.push_str(&format!("- Total LoC: {n}\n"));
    }

    let langs = metrics
        .map(|m| m.primary_languages(4))
        .filter(|l| !l.is_empty())
        .or_else(|| {
            scan.map(|s| s.primary_languages(4))
                .filter(|l| !l.is_empty())
        });
    if let Some(l) = langs {
        out.push_str(&format!("- Primary languages: {}\n", l.join(", ")));
    }

    if let Some(m) = metrics {
        out.push_str(&format!(
            "- Files: {}; Functions: {}\n",
            m.counts.files, m.counts.functions
        ));
    } else if let Some(s) = scan.filter(|s| s.file_count > 0) {
        out.push_str(&format!("- Files: {}\n", s.file_count));
    }

    // #6030: the component evidence — which manifests exist, what project each
    // declares, and its top declared dependencies.
    if let Some(fw) = scan
        .map(super::reporter_fill::format_frameworks)
        .filter(|f| !f.is_empty())
    {
        out.push_str(&format!("- Build manifests / frameworks: {fw}\n"));
    }
    out
}

/// Build the user-message data digest — per-application profile numbers plus
/// the COMPACT findings digest (#2357 follow-up; never the full prose).
///
/// Why: the model must see every deterministic figure it may cite, and every
/// finding it may reference or must elaborate, WITHOUT the evidence/business-
/// impact/remediation prose that already lives in the report body once
/// verified — that separation is what keeps this call's input AND output
/// bounded regardless of how many findings exist.
/// What: writes a report header, then per application its profile bullets
/// (LoC, languages, files/functions, and the detected build manifests — see
/// [`repo_profile_lines`]); then ONE combined compact-findings context
/// section and ONE elaboration-targets section (see
/// [`super::synthesize_digest`]); then the wave-3 coverage summary
/// (unchanged) and the analyst focus directives (unchanged, additive on top
/// of whichever section instructions are active).
/// `elaboration_cap` is the active [`SynthesisTier`]'s (#6093), so the digest's
/// target list and the schema's `maxItems` are always the same number.
/// Test: `synthesize_tests.rs::{prompt_excludes_greens, digest_stays_bounded_at_100_findings}`.
fn build_digest(model: &ReportModel, elaboration_cap: usize) -> String {
    let mut msg = String::with_capacity(4096);
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
        // #6029/#6030: `metrics` arrives only from a `--analyze` run, but
        // `scan` is built on every model build. Reading metrics alone printed
        // "No metrics available" for an ordinary sweep, leaving the executive
        // summary nothing to ground a purpose or component claim on. Same
        // metrics-then-scan precedence as `reporter_fill::fill_profile`.
        let profile = repo_profile_lines(repo);
        if profile.is_empty() {
            msg.push_str("- No metrics available for this application.\n");
        } else {
            msg.push_str(&profile);
        }
        // #5453/#6004: the authorship_summary slot has no other route to real
        // data — model.ticketing's precedent plumbs a figure to the TEMPLATE
        // but never the PROMPT (issue #5453), which starves the narrative of
        // anything to ground itself in. Fed here, not into the findings
        // digest, since it is its own section, not a Top-Risks input.
        if let Some(a) = &repo.authorship {
            msg.push_str(&format!(
                "- Authorship: {} distinct author(s), bus factor {}, top author {:.0}% of \
                 touches, single-author subsystems: {}\n",
                a.distinct_authors,
                a.bus_factor,
                a.top_author_share_pct,
                if a.single_author_subsystems.is_empty() {
                    "none".to_string()
                } else {
                    a.single_author_subsystems.join(", ")
                }
            ));
            if let Some(trend) = a.trajectory_summary() {
                msg.push_str(&format!("- Authorship trajectory: {trend}\n"));
            }
        }
        msg.push('\n');
    }

    // #6147: the architecture section's one deterministic input. Stated as its
    // own block rather than as per-repository profile bullets because the
    // architecture paragraph reads it as a whole — a member count means little
    // without the edge count and the shared core beside it. Empty (and so
    // absent) unless a repository declared one.
    msg.push_str(&super::topology::prompt_facts(model));

    // #2357 follow-up: ONE combined compact-findings digest across all repos,
    // replacing the old per-repo full-title bullet list. Greens are excluded
    // structurally by `gather_compact_findings` (the no-green rule, unchanged).
    let compact = gather_compact_findings(model);
    let split = build_split_with_cap(&compact, elaboration_cap);
    msg.push_str(&render_context_section(&split));
    msg.push_str(&render_elaboration_section(&split));

    // #2357: when the repo-evidence investigation ran, feed synthesis the real
    // coverage so the exec summary can only claim a data gap that truly exists —
    // and must name it.
    if let Some(inv) = &model.investigation {
        msg.push('\n');
        msg.push_str(&inv.coverage_prompt_summary());
        msg.push('\n');
    }

    // #2340: analyst focus directives are an ADDITIVE per-run overlay on top of
    // whichever section instructions are active (generic or template-overridden)
    // — they steer EMPHASIS only and never relax the grounding rules (the
    // numeric guardrail and no-green exclusion still bind).
    if let Some(instructions) = &model.instructions {
        msg.push_str("\n## Analyst focus directives (steer EMPHASIS only — never invent to satisfy them)\n\n");
        msg.push_str(instructions);
        msg.push('\n');
    }

    msg.push_str(
        "\nSynthesise the narrative sections from the data above, obeying every rule in the system prompt. Where the analyst focus directives above are relevant, weight the executive summary and top risks toward them — but only using figures and findings actually present in the data.\n",
    );
    msg
}
