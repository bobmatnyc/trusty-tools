//! Config, stats, tool schema, and argument parsing/validation for the
//! tool-calling dream-consolidation pass (epic #2866).
//!
//! Why: The legacy semantic-consolidation phase parses free-text LLM output
//! (`parse_consolidation_actions`), which silently degrades to an empty
//! action list on any malformed reply. Structured tool-calling gives the
//! model a typed contract (`emit_consolidation`) and gives us a hard
//! validation boundary before any palace mutation happens.
//! What: `DreamConsolidationConfig` (default OFF), `DreamConsolidationStats`
//! (per-pass telemetry, serialisable into `DreamStats`), the
//! `emit_consolidation` `ToolDef` builder, the typed output structs
//! (`ConsolidationOutput`, `ConsolidationFact`), and
//! `parse_emit_consolidation` — strict serde parsing + semantic validation
//! (non-empty triple parts, confidence clamped to `[0.0, 1.0]`).
//! Test: `dream_consolidation::tests` — parse round-trips, malformed JSON,
//! missing fields, out-of-range confidence, empty triple parts.

use serde::{Deserialize, Serialize};

/// Name of the single tool the consolidation model is allowed to call.
///
/// Why: The pass matches streamed `ChatEvent::ToolCall` events by name; a
/// shared constant keeps the schema builder and the collector in lock-step.
/// What: `"emit_consolidation"`, passed verbatim as `ToolDef::name`.
/// Test: `tool_def_has_expected_shape`.
pub const EMIT_CONSOLIDATION_TOOL: &str = "emit_consolidation";

/// Provenance tag written on every KG triple this pass asserts.
///
/// Why: Distinguishes tool-calling consolidation output from the legacy
/// free-text pass (`"dream:semantic_consolidation"`, `cycle.rs`) and from
/// hand-asserted facts, so the two generations remain separately auditable.
/// What: `"dream:llm_cluster_consolidation"` per epic #2866 (supersedes the
/// spec draft's `"dream:structured_consolidation"` placeholder).
/// Test: `pass_stores_summary_facts_and_tombstones` asserts the tag on both
/// fact and tombstone triples.
pub const DREAM_CONSOLIDATION_PROVENANCE: &str = "dream:llm_cluster_consolidation";

/// Tag applied to every summary drawer created by this pass.
///
/// Why: Summary drawers must be distinguishable from user-authored content
/// (spec §3) so operators can list, audit, or bulk-forget machine-generated
/// summaries without touching originals.
/// What: `"dream:consolidation"`, pushed into `Drawer::tags` on the summary.
/// Test: `pass_stores_summary_facts_and_tombstones` finds the summary by tag.
pub const DREAM_SUMMARY_TAG: &str = "dream:consolidation";

/// KG predicate marking a drawer as archived (tombstoned) by consolidation.
///
/// Why: Reuses the exact triple shape the legacy pass already writes
/// (`record_provenance_and_collect_superseded`) so archived-ness is one
/// uniform query across both consolidation generations (spec §4.2).
/// What: `"superseded_by"`; a drawer is archived iff it is the subject of an
/// ACTIVE (`valid_to == None`) triple with this predicate.
/// Test: `recall_excludes_tombstoned_sources`.
pub const SUPERSEDED_BY_PREDICATE: &str = "superseded_by";

/// Configuration for the tool-calling dream-consolidation pass.
///
/// Why: The pass spends real money per LLM call and mutates durable memory
/// (summary drawers, KG facts, tombstones), so it ships default-OFF and every
/// cost lever is operator-tunable without recompiling (spec §5).
/// What: `enabled` gates the whole pass (default `false`); `model` is the
/// OpenRouter/Ollama model id (default `anthropic/claude-haiku-4-5`, resolved
/// by Bob 2026-07-16); `max_batch_size` caps drawers per cluster prompt;
/// `max_calls_per_cycle` is the hard per-cycle LLM-call ceiling (mirrors the
/// `SemanticConsolidationConfig` precedent). Serde defaults let the struct
/// deserialize from a partial `[dream_consolidation]` TOML section.
/// Test: `config_defaults_are_off_and_haiku`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DreamConsolidationConfig {
    /// Whether the pass runs at all. Default `false` (PoC is opt-in only).
    #[serde(default)]
    pub enabled: bool,
    /// Model id for the tool-calling consolidation prompts.
    #[serde(default = "default_model")]
    pub model: String,
    /// Maximum drawers in a single cluster prompt.
    #[serde(default = "default_max_batch_size")]
    pub max_batch_size: usize,
    /// Hard ceiling on LLM calls per pass invocation (cost control).
    #[serde(default = "default_max_calls_per_cycle")]
    pub max_calls_per_cycle: usize,
}

fn default_model() -> String {
    "anthropic/claude-haiku-4-5".to_string()
}

fn default_max_batch_size() -> usize {
    8
}

fn default_max_calls_per_cycle() -> usize {
    20
}

impl Default for DreamConsolidationConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            model: default_model(),
            max_batch_size: default_max_batch_size(),
            max_calls_per_cycle: default_max_calls_per_cycle(),
        }
    }
}

/// Per-pass telemetry for the tool-calling consolidation pass.
///
/// Why: Operators need to see whether the pass fired, how many LLM calls it
/// spent, and what it actually changed — especially while the feature is
/// opt-in and being validated against real palaces.
/// What: Plain counters, embedded in `DreamStats` (`#[serde(default)]` keeps
/// older `dream_stats.json` snapshots readable) and returned by the
/// `dream_consolidate_room` MCP tool. `no_tool_call` counts model replies
/// that never invoked the tool (a defined no-op, not an error); `errors`
/// counts provider/parse/storage failures that were swallowed fail-open.
/// Test: `pass_stores_summary_facts_and_tombstones`,
/// `pass_counts_no_tool_call_as_noop`, `pass_swallows_provider_error`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DreamConsolidationStats {
    /// Clusters submitted to the model.
    #[serde(default)]
    pub clusters_processed: usize,
    /// LLM calls made (each cluster invocation counts once).
    #[serde(default)]
    pub llm_calls: usize,
    /// Summary drawers created.
    #[serde(default)]
    pub summaries_created: usize,
    /// Freeform inferences folded into summary drawers.
    #[serde(default)]
    pub inferences_recorded: usize,
    /// KG fact triples asserted.
    #[serde(default)]
    pub facts_asserted: usize,
    /// Facts dropped by validation (empty triple part / non-finite confidence).
    #[serde(default)]
    pub facts_dropped: usize,
    /// Source drawers tombstoned via `superseded_by`.
    #[serde(default)]
    pub sources_tombstoned: usize,
    /// Model replies that completed without calling the tool (no-op clusters).
    #[serde(default)]
    pub no_tool_call: usize,
    /// Provider / parse / storage failures swallowed fail-open.
    #[serde(default)]
    pub errors: usize,
}

/// A validated subject–predicate–object fact from the model.
///
/// Why: The raw wire fact is untrusted; this type only exists post-validation
/// so storage code cannot accidentally assert an empty triple part.
/// What: Trimmed, non-empty strings plus a confidence already clamped to
/// `[0.0, 1.0]`.
/// Test: `parse_clamps_out_of_range_confidence`, `parse_drops_empty_triple_parts`.
#[derive(Debug, Clone, PartialEq)]
pub struct ConsolidationFact {
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub confidence: f32,
}

/// Validated output of one `emit_consolidation` tool call.
///
/// Why: Gives the pass a typed, already-validated unit of work; anything that
/// failed validation was either rejected wholesale (`EmitParseError`) or
/// dropped per-fact and counted in `facts_dropped`.
/// What: `summary` is guaranteed non-empty after trimming; `inferences` has
/// empty entries removed; `facts` passed the triple-part and confidence
/// checks; `facts_dropped` counts individually rejected facts.
/// Test: `parse_round_trips_valid_payload`.
#[derive(Debug, Clone, PartialEq)]
pub struct ConsolidationOutput {
    pub summary: String,
    pub inferences: Vec<String>,
    pub facts: Vec<ConsolidationFact>,
    pub facts_dropped: usize,
}

/// Validation failure for `emit_consolidation` tool-call arguments.
///
/// Why: `ToolCall.arguments` is a raw string by design (models emit malformed
/// JSON); the pass must distinguish "unparseable" from "parsed but invalid"
/// so both are logged usefully and neither ever partially mutates the palace.
/// What: `Json` wraps the serde error (malformed JSON or missing required
/// fields); `EmptySummary` rejects a structurally valid call whose summary is
/// blank — a summary-less consolidation has nothing to store.
/// Test: `parse_rejects_malformed_json`, `parse_rejects_missing_summary`,
/// `parse_rejects_blank_summary`.
#[derive(Debug, thiserror::Error)]
pub enum EmitParseError {
    #[error("malformed emit_consolidation arguments: {0}")]
    Json(#[from] serde_json::Error),
    #[error("emit_consolidation summary is empty after trimming")]
    EmptySummary,
}

/// Wire shape of the tool-call arguments (untrusted, pre-validation).
#[derive(Deserialize)]
struct RawOutput {
    summary: String,
    #[serde(default)]
    inferences: Vec<String>,
    #[serde(default)]
    facts: Vec<RawFact>,
}

/// Wire shape of one fact (untrusted, pre-validation).
#[derive(Deserialize)]
struct RawFact {
    subject: String,
    predicate: String,
    object: String,
    confidence: f32,
}

/// Parse and validate raw `emit_consolidation` tool-call arguments.
///
/// Why: The model's arguments string is untrusted input on the write path to
/// durable memory; every invariant the storage code relies on is enforced
/// here, in one place, before anything touches the palace.
/// What: Strict `serde_json` parse (missing `summary` or `confidence` fields
/// fail the whole call), then: summary must be non-empty after trim (else
/// `EmptySummary`); blank inference strings are dropped silently; each fact
/// must have non-empty trimmed subject/predicate/object AND a finite
/// confidence, else the fact is dropped and counted in `facts_dropped`;
/// surviving confidences are clamped to `[0.0, 1.0]`.
/// Test: `parse_round_trips_valid_payload`, `parse_rejects_malformed_json`,
/// `parse_rejects_missing_summary`, `parse_rejects_blank_summary`,
/// `parse_clamps_out_of_range_confidence`, `parse_drops_empty_triple_parts`,
/// `parse_drops_non_finite_confidence`.
pub fn parse_emit_consolidation(arguments: &str) -> Result<ConsolidationOutput, EmitParseError> {
    let raw: RawOutput = serde_json::from_str(arguments)?;

    let summary = raw.summary.trim().to_string();
    if summary.is_empty() {
        return Err(EmitParseError::EmptySummary);
    }

    let inferences: Vec<String> = raw
        .inferences
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    let mut facts = Vec::with_capacity(raw.facts.len());
    let mut facts_dropped = 0usize;
    for f in raw.facts {
        let subject = f.subject.trim().to_string();
        let predicate = f.predicate.trim().to_string();
        let object = f.object.trim().to_string();
        if subject.is_empty() || predicate.is_empty() || object.is_empty() {
            facts_dropped += 1;
            continue;
        }
        if !f.confidence.is_finite() {
            facts_dropped += 1;
            continue;
        }
        facts.push(ConsolidationFact {
            subject,
            predicate,
            object,
            confidence: f.confidence.clamp(0.0, 1.0),
        });
    }

    Ok(ConsolidationOutput {
        summary,
        inferences,
        facts,
        facts_dropped,
    })
}

/// Build the `emit_consolidation` tool definition (spec §3.3).
///
/// Why: The schema is the model-facing contract; keeping it next to the
/// parser that consumes the arguments makes drift between the two obvious in
/// review.
/// What: OpenAI-function-shaped `ToolDef` with a JSON-Schema `parameters`
/// object requiring `summary`, `inferences`, and `facts` (each fact requires
/// subject/predicate/object/confidence, confidence bounded `[0.0, 1.0]`).
/// Test: `tool_def_has_expected_shape`.
pub fn emit_consolidation_tool() -> crate::chat::ToolDef {
    crate::chat::ToolDef {
        name: EMIT_CONSOLIDATION_TOOL.to_string(),
        description: "Report the result of consolidating a cluster of related memories: \
                      a short summary, any additional inferences drawn from reading them \
                      together, and any subject-predicate-object facts that can be \
                      asserted with confidence."
            .to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "summary": {
                    "type": "string",
                    "description": "A single, standalone paragraph that captures everything worth keeping from this cluster. Must not lose any fact a reader would need."
                },
                "inferences": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Additional conclusions that follow from reading the cluster together but are not stated in any single source memory. Empty array if none."
                },
                "facts": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "subject":    { "type": "string" },
                            "predicate":  { "type": "string" },
                            "object":     { "type": "string" },
                            "confidence": { "type": "number", "minimum": 0.0, "maximum": 1.0 }
                        },
                        "required": ["subject", "predicate", "object", "confidence"]
                    },
                    "description": "Ontological facts as (subject, predicate, object) triples derivable from this cluster, each with the model's own confidence. Empty array if none."
                }
            },
            "required": ["summary", "inferences", "facts"]
        }),
    }
}
