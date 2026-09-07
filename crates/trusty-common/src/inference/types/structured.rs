//! [`StructuredOutput`] — the neutral schema-constrained-response directive (#5588).
//!
//! Why: `InferenceAdapter::supports_structured_output` advertised a capability
//! no request could carry, so a caller that needed schema-valid JSON had to
//! render its JSON Schema as prose in the system message and hope the model
//! complied. This type is the request-level field that closes that gap.
//! What: [`StructuredOutput`] carries the schema, the name providers attach to
//! it, and the strict flag. Two methods render it into the only two wire
//! dialects the workspace speaks — [`StructuredOutput::openai_response_format`]
//! for the OpenAI-compatible `response_format` field and
//! [`StructuredOutput::anthropic_output_config`] for the Anthropic Messages API
//! `output_config` field. The struct is deliberately dialect-free so a third
//! provider adds a renderer here rather than a field. #7082: the OpenAI
//! renderer normalizes a strict schema through
//! [`super::strict_json_schema`] on the way out, because that dialect
//! validates the schema document as well as the completion.
//! Test: inline `tests` — `openai_envelope_carries_name_strict_and_schema`,
//! `openai_envelope_normalizes_a_strict_schema`,
//! `openai_envelope_leaves_a_lenient_schema_verbatim`,
//! `anthropic_envelope_carries_only_the_schema`,
//! `strict_defaults_true_and_round_trips`.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::strict_schema::strict_json_schema;

/// A JSON-Schema-constrained response directive attached to a
/// [`super::ChatRequest`].
///
/// Why: schema enforcement has to travel with the request, not the prompt — a
/// schema described in prose is a suggestion, and #5588 was filed because the
/// difference is observable as malformed model output.
/// What: `name` is the label OpenAI-dialect providers require on the schema
/// (Anthropic ignores it); `schema` is the JSON Schema object itself; `strict`
/// asks the provider to reject any completion that does not validate. Construct
/// with [`StructuredOutput::new`] (the struct is `#[non_exhaustive]`, so a later
/// provider knob is additive) and relax with [`StructuredOutput::lenient`].
/// Test: `openai_envelope_carries_name_strict_and_schema`,
/// `strict_defaults_true_and_round_trips`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct StructuredOutput {
    /// The schema's name, required by OpenAI-dialect providers.
    pub name: String,
    /// The JSON Schema the response must validate against.
    pub schema: Value,
    /// Whether the provider must reject a non-validating completion.
    #[serde(default = "default_strict")]
    pub strict: bool,
}

/// `strict` defaults to `true` — a schema attached without one is a constraint,
/// not a hint.
fn default_strict() -> bool {
    true
}

impl StructuredOutput {
    /// Build a strict directive from a schema name and a JSON Schema.
    ///
    /// Why: strict is the reason a caller reaches for this type at all, so it is
    /// the default rather than a second argument every call site repeats.
    /// What: sets `name` and `schema`; `strict` is `true`.
    /// Test: `openai_envelope_carries_name_strict_and_schema`.
    pub fn new(name: impl Into<String>, schema: Value) -> Self {
        Self {
            name: name.into(),
            schema,
            strict: true,
        }
    }

    /// The same directive with `strict` cleared.
    ///
    /// Why: strict schema enforcement is model-dependent on some OpenAI-dialect
    /// gateways — a caller that would rather get best-effort JSON than a 422
    /// needs a way to say so.
    /// What: consumes `self` and returns it with `strict = false`.
    /// Test: `lenient_clears_strict`.
    #[must_use]
    pub fn lenient(mut self) -> Self {
        self.strict = false;
        self
    }

    /// Render the OpenAI-compatible `response_format` value.
    ///
    /// Why: OpenRouter, Fireworks, OpenAI-direct, Together and AtlasCloud all
    /// take the same `{type:"json_schema", json_schema:{…}}` envelope, so the
    /// shared OpenAI-compat adapter renders it once here. #7082: with `strict`
    /// set, those providers also validate the schema document and reject any
    /// object node that leaves `additionalProperties` open or under-fills
    /// `required` — so this is where the schema is normalized, and every caller
    /// gets a strict-valid document without editing its own schema builder.
    /// What: returns
    /// `{"type":"json_schema","json_schema":{"name":…,"strict":…,"schema":…}}`.
    /// When `strict` is set, `schema` is [`strict_json_schema`] applied to the
    /// caller's schema (a copy — `self` is untouched). Normalization is
    /// unconditional for a strict directive rather than opt-in: the rules it
    /// enforces are exactly what the provider demands there, and the same
    /// omission has now shipped three times (#1235, #5675, #7082) precisely
    /// because it was each builder's job to remember. With
    /// [`StructuredOutput::lenient`] the schema travels verbatim — no provider
    /// requires the rules then, and forcing every optional property into
    /// `required` would narrow a schema whose author asked for best-effort JSON.
    /// Test: `openai_envelope_carries_name_strict_and_schema`,
    /// `openai_envelope_normalizes_a_strict_schema`,
    /// `openai_envelope_leaves_a_lenient_schema_verbatim`.
    pub fn openai_response_format(&self) -> Value {
        // #7082: strict mode is validated against the schema document itself.
        let schema = if self.strict {
            strict_json_schema(&self.schema)
        } else {
            self.schema.clone()
        };
        json!({
            "type": "json_schema",
            "json_schema": {
                "name": self.name,
                "strict": self.strict,
                "schema": schema,
            }
        })
    }

    /// Render the Anthropic Messages API `output_config` value.
    ///
    /// Why: Anthropic constrains output through `output_config.format`, whose
    /// envelope carries the schema alone — it has no slot for a schema name and
    /// no strict toggle (enforcement is unconditional).
    /// What: returns `{"format":{"type":"json_schema","schema":…}}`. `name` and
    /// `strict` are deliberately dropped: sending either is a 400.
    /// Test: `anthropic_envelope_carries_only_the_schema`.
    pub fn anthropic_output_config(&self) -> Value {
        json!({
            "format": {
                "type": "json_schema",
                "schema": self.schema,
            }
        })
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn schema() -> Value {
        json!({
            "type": "object",
            "properties": { "verdict": { "type": "string" } },
            "required": ["verdict"],
            "additionalProperties": false,
        })
    }

    /// Why: the OpenAI envelope is what five providers put on the wire; a wrong
    /// key or a missing `strict` is a 400 the caller cannot diagnose.
    /// Test: itself.
    #[test]
    fn openai_envelope_carries_name_strict_and_schema() {
        let v = StructuredOutput::new("review_output", schema()).openai_response_format();
        assert_eq!(v["type"], "json_schema");
        assert_eq!(v["json_schema"]["name"], "review_output");
        assert_eq!(v["json_schema"]["strict"], true);
        assert_eq!(v["json_schema"]["schema"]["type"], "object");
    }

    /// Why (#7082): OpenRouter rejected every `tga profile` call with `'addition\
    /// alProperties' is required to be supplied and to be false`, because the
    /// nested `items` object of a hand-written schema left it open. The renderer
    /// closes it, so a caller's builder no longer has to.
    /// Test: itself.
    #[test]
    fn openai_envelope_normalizes_a_strict_schema() {
        let loose = json!({
            "type": "object",
            "properties": {
                "findings": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "kind": {"type": "string"},
                            "severity": {"type": "string"}
                        },
                        "required": ["kind"]
                    }
                }
            }
        });
        let directive = StructuredOutput::new("period_findings", loose.clone());
        let sent = directive.openai_response_format();
        let items = &sent["json_schema"]["schema"]["properties"]["findings"]["items"];
        assert_eq!(items["additionalProperties"], json!(false), "{sent}");
        assert_eq!(items["required"], json!(["kind", "severity"]), "{sent}");
        assert_eq!(
            sent["json_schema"]["schema"]["additionalProperties"],
            json!(false),
            "{sent}"
        );
        assert_eq!(
            directive.schema, loose,
            "the caller's schema must not change"
        );
    }

    /// Why: `lenient` exists for gateways whose strict support is
    /// model-dependent; normalizing there would force every optional property
    /// into `required` for a caller who asked for best-effort JSON.
    /// Test: itself.
    #[test]
    fn openai_envelope_leaves_a_lenient_schema_verbatim() {
        let loose = json!({
            "type": "object",
            "properties": {"a": {"type": "string"}, "b": {"type": "string"}},
            "required": ["a"]
        });
        let sent = StructuredOutput::new("x", loose.clone())
            .lenient()
            .openai_response_format();
        assert_eq!(sent["json_schema"]["schema"], loose, "{sent}");
    }

    /// Why: Anthropic rejects a `name` or `strict` inside `output_config.format`
    /// — dropping them is the difference between a constrained call and a 400.
    /// Test: itself.
    #[test]
    fn anthropic_envelope_carries_only_the_schema() {
        let v = StructuredOutput::new("review_output", schema()).anthropic_output_config();
        assert_eq!(v["format"]["type"], "json_schema");
        assert_eq!(v["format"]["schema"]["type"], "object");
        assert!(v["format"].get("name").is_none(), "{v}");
        assert!(v["format"].get("strict").is_none(), "{v}");
    }

    /// Why: a schema deserialised without `strict` must still constrain — the
    /// serde default is what keeps an older stored request strict.
    /// Test: itself.
    #[test]
    fn strict_defaults_true_and_round_trips() {
        let parsed: StructuredOutput =
            serde_json::from_value(json!({ "name": "x", "schema": { "type": "object" } }))
                .expect("deserialise");
        assert!(parsed.strict);

        let round: StructuredOutput =
            serde_json::from_value(serde_json::to_value(&parsed).expect("serialise"))
                .expect("re-deserialise");
        assert_eq!(round, parsed);
    }

    /// Why: `lenient` is the documented escape hatch for gateways whose strict
    /// support is model-dependent.
    /// Test: itself.
    #[test]
    fn lenient_clears_strict() {
        let s = StructuredOutput::new("x", schema()).lenient();
        assert!(!s.strict);
        assert_eq!(s.openai_response_format()["json_schema"]["strict"], false);
    }
}
