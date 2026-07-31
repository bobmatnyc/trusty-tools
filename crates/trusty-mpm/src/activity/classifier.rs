//! The production activity classifier, backed by the unified inference adapter.
//!
//! Why (#4427, epic #4429): the activity classifier was the last per-session LLM
//! caller in this crate still driving the legacy `trusty_common::chat::ChatProvider`
//! SSE pump. Every other trusty-mpm LLM path (the Layer-3 manager, see
//! `daemon::manager::inference`) already resolves providers through the unified
//! `trusty_common::inference` adapter + `Configurator` credential ladder, so the
//! classifier's bespoke `OpenRouterProvider` + hand-accumulated stream was the odd
//! one out: two credential policies, two error taxonomies, and a streaming
//! transport whose partial output the classifier immediately discarded. This
//! module is the migrated seam — one blocking `InferenceAdapter::chat` call per
//! classification.
//! What: [`OpenRouterClassifier`] implements [`LlmClassifier`] by resolving an
//! `Arc<dyn InferenceAdapter>` (credential-backed in production, injected in
//! tests) and issuing exactly ONE [`ChatRequest`], then parsing the model's JSON
//! verdict with [`extract_json`]/[`parse_state`]. It is deliberately split out of
//! `monitor.rs` so both files stay under the 500-SLOC production cap.
//! Test: `crates/trusty-mpm/src/activity/classifier_tests.rs` — the whole file,
//! notably `classify_parses_verdict_and_usage`,
//! `classify_missing_credential_maps_to_missing_api_key`,
//! `classify_adapter_error_maps_to_llm`,
//! `classify_unparseable_output_maps_to_serialization`.

use std::sync::Arc;

use trusty_common::inference::credentials::{KeyStore, default_store};
use trusty_common::inference::{
    ChatMessage, ChatRequest, Configurator, InferenceAdapter, InferenceError,
    register_default_factories,
};

use super::cache::{ActivityState, ActivityVerdict};
use super::monitor::{ActivityError, LlmClassifier};

/// Environment key selecting the classifier's model slug.
///
/// Why: operators switch between a cheap and a capable model without
/// recompiling; naming the key as a constant keeps the classifier, the docs
/// (`docs/reference/environment-variables.md`), and the tests in agreement.
/// What: read by [`OpenRouterClassifier::new`]; shared fleet-wide with the
/// Layer-3 manager's fallback key.
/// Test: `model_defaults_when_env_unset`.
pub const CLASSIFIER_MODEL_ENV: &str = "TRUSTY_LLM_MODEL";

/// The model slug used when [`CLASSIFIER_MODEL_ENV`] is unset.
///
/// Why: a cheap, fast model is the right default for a one-shot six-way
/// classification that runs on every session poll. The bare `openai/…` slug is
/// unchanged from the pre-#4427 classifier, so no operator config moves.
/// What: the fallback slug handed to `Configurator::build`.
/// Test: `model_defaults_when_env_unset`.
pub const DEFAULT_CLASSIFIER_MODEL: &str = "openai/gpt-4o-mini";

/// How the classifier obtains an [`InferenceAdapter`].
///
/// Why: production must resolve a provider from the operator's credential store
/// on EVERY call — a daemon started before the key was configured has to start
/// working the moment it is, exactly as the old per-call `std::env::var` read
/// did. Tests need a deterministic adapter with no network or credential. One
/// enum keeps [`OpenRouterClassifier::adapter`] a single path. Mirrors
/// `daemon::manager::inference::Source`.
/// What: [`Self::Credentialed`] holds the factory registry and the resolved key
/// store; [`Self::Fixed`] holds a ready adapter.
/// Test: `classify_missing_credential_maps_to_missing_api_key` (credentialed),
/// `classify_parses_verdict_and_usage` (fixed).
enum Source {
    /// Build an adapter from the model slug + credential on each call (production).
    Credentialed {
        /// Factory registry with all default HTTP providers registered.
        configurator: Configurator,
        /// Resolved credential store (env > `.env.local` > secure store).
        store: Box<dyn KeyStore>,
    },
    /// A fixed, pre-built adapter (test injection).
    Fixed(Arc<dyn InferenceAdapter>),
}

/// The [`LlmClassifier`] that asks a real model what a session is doing.
///
/// Why: the `/activity` route, the TUI per-session summary, and the circuit
/// breaker all need a state verdict for a pane of terminal text; this is the one
/// production implementation behind that trait.
/// What: owns the model slug plus a [`Source`]. Named `OpenRouterClassifier` for
/// source compatibility with its callers — post-#4427 the concrete provider is
/// whatever the shared credential ladder resolves for the slug, which is
/// OpenRouter for every slug whose own provider family has no key configured.
/// Test: the `classify_*` cases in `classifier_tests.rs`.
pub struct OpenRouterClassifier {
    /// Model slug sent on every [`ChatRequest`] and resolved to a provider.
    model: String,
    /// Adapter-resolution strategy.
    source: Source,
}

impl OpenRouterClassifier {
    /// Construct the production classifier over the operator's credential store.
    ///
    /// Why: the daemon builds exactly one of these lazily (`DaemonState::activity_monitor`)
    /// and the supervisor one more; both want "whatever provider this operator has
    /// configured", resolved per call so a key added after startup takes effect.
    /// What: registers the default provider factories into a fresh [`Configurator`],
    /// resolves [`default_store`], and reads the model from [`CLASSIFIER_MODEL_ENV`]
    /// (default [`DEFAULT_CLASSIFIER_MODEL`]). Performs NO network or credential
    /// lookup itself — construction cannot fail.
    /// Test: `model_defaults_when_env_unset`, `model_reads_env_override`.
    pub fn new() -> Self {
        // #4427: was `OpenRouterProvider::new(env_key, model)`; the configurator
        // replaces the env-only key read with the shared credential ladder.
        let mut configurator = Configurator::new();
        register_default_factories(&mut configurator);
        Self {
            model: resolve_classifier_model(),
            source: Source::Credentialed {
                configurator,
                store: default_store(),
            },
        }
    }

    /// Construct a classifier bound to a fixed, pre-built adapter.
    ///
    /// Why: the unit suite must drive [`Self::classify`] itself — the parse, the
    /// error mapping, the usage plumbing — with zero network and zero credential
    /// (the hermetic bar the manager suite already meets). This is that seam.
    /// What: stores `adapter` as [`Source::Fixed`] and `model` verbatim.
    /// Test: every `classify_*` case that scripts a response.
    pub fn with_adapter(adapter: Arc<dyn InferenceAdapter>, model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            source: Source::Fixed(adapter),
        }
    }

    /// The model slug this classifier will request.
    ///
    /// Why: `ActivityMonitor` records the model in its per-check metrics, and the
    /// tests assert the env-resolution ladder without reaching into the field.
    /// What: borrows the stored slug.
    /// Test: `model_defaults_when_env_unset`, `model_reads_env_override`.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Resolve a live adapter, or fail fast with a typed [`ActivityError`].
    ///
    /// Why: the credential check MUST happen before any network call so an
    /// unconfigured daemon degrades to an `Unknown` verdict instead of burning a
    /// request — the pre-#4427 `std::env::var(…)?` guarantee, preserved. Building
    /// is synchronous (`Configurator::build` never awaits), so this stays a plain
    /// non-async helper.
    /// What: clones a fixed adapter, or runs [`Configurator::build`] against the
    /// model slug and store. [`InferenceError::MissingCredential`] maps to
    /// [`ActivityError::MissingApiKey`] (what callers already branch on); any
    /// other construction failure maps to [`ActivityError::Llm`].
    /// Test: `classify_missing_credential_maps_to_missing_api_key`.
    fn adapter(&self) -> Result<Arc<dyn InferenceAdapter>, ActivityError> {
        match &self.source {
            Source::Fixed(adapter) => Ok(Arc::clone(adapter)),
            Source::Credentialed {
                configurator,
                store,
            } => configurator
                .build(&self.model, store.as_ref())
                .map(Arc::from)
                .map_err(map_inference_error),
        }
    }
}

impl Default for OpenRouterClassifier {
    /// Delegate to [`OpenRouterClassifier::new`].
    fn default() -> Self {
        Self::new()
    }
}

impl LlmClassifier for OpenRouterClassifier {
    /// Ask the model to classify one pane of terminal text.
    ///
    /// Why: this is the single LLM call the activity subsystem makes. It is
    /// deliberately BLOCKING, not streaming (#4427): the previous implementation
    /// pumped SSE deltas only to concatenate them and parse the finished string,
    /// so streaming bought nothing and cost a truncated-mid-stream failure mode
    /// (#3757) that had to be detected and re-reported by hand. One request, one
    /// response, one parse.
    /// What: resolves an adapter (fail-fast on a missing credential), sends the
    /// classification prompt as a single `user` turn, takes
    /// [`trusty_common::inference::ChatResponse::first_text`], and parses the JSON
    /// verdict. Returns `(verdict, prompt_tokens, completion_tokens)` from the
    /// provider-reported usage.
    /// Test: `classify_parses_verdict_and_usage`, `classify_sends_one_user_turn`,
    /// `classify_adapter_error_maps_to_llm`,
    /// `classify_unparseable_output_maps_to_serialization`.
    async fn classify(
        &self,
        pane_text: &str,
    ) -> Result<(ActivityVerdict, u32, u32), ActivityError> {
        // #4427: resolve (and therefore credential-check) before the request is
        // even built — nothing touches the network until this succeeds.
        let adapter = self.adapter()?;

        let request = ChatRequest::new(
            self.model.clone(),
            vec![ChatMessage::user(classification_prompt(pane_text))],
        );

        // #4427: one blocking call replaces the `chat_stream` + mpsc pump. A
        // provider/transport failure is `ActivityError::Llm`, exactly as the old
        // `send_result` / `ChatEvent::Error` arms produced.
        let response = adapter.chat(&request).await.map_err(map_inference_error)?;

        let text = response.first_text().unwrap_or_default();
        let json_str = extract_json(&text).unwrap_or(&text);
        let parsed: serde_json::Value = serde_json::from_str(json_str).map_err(|e| {
            ActivityError::Serialization(format!("parse failed: {e} — raw: {text}"))
        })?;

        let state = parse_state(parsed["state"].as_str().unwrap_or("unknown"));
        let summary = parsed["summary"]
            .as_str()
            .unwrap_or("no summary")
            .to_owned();
        let confidence = parsed["confidence"].as_f64().unwrap_or(0.5) as f32;

        // #4427: the adapter returns normalized usage, so the per-check metrics
        // finally carry real token counts. The SSE path could not see them and
        // hard-coded (0, 0), which made every activity cost tally read zero.
        let usage = response.usage();
        Ok((
            ActivityVerdict {
                state,
                summary,
                confidence,
            },
            usage.prompt_tokens,
            usage.completion_tokens,
        ))
    }
}

/// Resolve the classifier's model slug from the environment.
///
/// Why: keeps the documented precedence in one place so construction and any
/// diagnostic read agree.
/// What: [`CLASSIFIER_MODEL_ENV`] when set and non-blank, else
/// [`DEFAULT_CLASSIFIER_MODEL`].
/// Test: `model_defaults_when_env_unset`, `model_reads_env_override`.
fn resolve_classifier_model() -> String {
    std::env::var(CLASSIFIER_MODEL_ENV)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_CLASSIFIER_MODEL.to_owned())
}

/// Map an [`InferenceError`] onto the activity subsystem's error taxonomy.
///
/// Why: callers (and `ActivityMonitor::check`'s degrade arm) branch on
/// [`ActivityError::MissingApiKey`] to return an `Unknown` verdict instead of
/// propagating a failure; that contract must survive the adapter migration
/// unchanged. Every other inference failure is a call failure.
/// What: [`InferenceError::MissingCredential`] → [`ActivityError::MissingApiKey`];
/// everything else → [`ActivityError::Llm`] carrying the error's display text
/// (which never contains a credential — see `InferenceError`'s own contract).
/// Test: `classify_missing_credential_maps_to_missing_api_key`,
/// `classify_adapter_error_maps_to_llm`.
fn map_inference_error(err: InferenceError) -> ActivityError {
    match err {
        InferenceError::MissingCredential { .. } => ActivityError::MissingApiKey,
        other => ActivityError::Llm(other.to_string()),
    }
}

/// Build the classification prompt for a pane of terminal text.
///
/// Why: extracted so the prompt is assertable in a test without issuing a call,
/// and so `classify` reads as transport rather than prompt-smithing. The wording
/// is byte-identical to the pre-#4427 prompt — the migration must not move the
/// model's behaviour.
/// What: instructs a JSON-only reply over the six valid states and embeds
/// `pane_text` in a fenced block.
/// Test: `classify_sends_one_user_turn`.
fn classification_prompt(pane_text: &str) -> String {
    format!(
        "Classify the activity state of this Claude Code terminal session.\n\
         Respond ONLY with valid JSON: {{\"state\": \"<state>\", \"summary\": \"<summary>\", \"confidence\": <0.0-1.0>}}\n\
         Valid states: working, idle, blocked_on_permission, errored, done, unknown\n\n\
         Terminal output (last 60 lines):\n```\n{pane_text}\n```"
    )
}

/// Extract the first `{…}` block from a response that may have prose around it.
///
/// Why: models routinely wrap JSON in markdown fences or prepend an apology;
/// slicing between the outermost braces recovers the payload instead of failing
/// the whole check.
/// What: returns the span from the first `{` to the LAST `}` (inclusive), or
/// `None` when either brace is absent or they appear out of order.
/// Test: `extract_json_plain`, `extract_json_from_fenced_prose`,
/// `extract_json_spans_to_last_brace`, `extract_json_none_without_braces`,
/// `extract_json_none_when_reversed`.
fn extract_json(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if end > start {
        Some(&text[start..=end])
    } else {
        None
    }
}

/// Parse a state string into an [`ActivityState`] variant.
///
/// Why: the model returns a snake_case string and may invent one; a total
/// function with an `Unknown` fallback keeps an off-script reply from failing the
/// check. Serde would reject the unexpected variant instead.
/// What: case-insensitive exact match over the six documented states; anything
/// else (including empty) is [`ActivityState::Unknown`].
/// Test: `parse_state_maps_every_documented_state`, `parse_state_is_case_insensitive`,
/// `parse_state_unknown_for_garbage`.
fn parse_state(s: &str) -> ActivityState {
    match s.to_ascii_lowercase().as_str() {
        "working" => ActivityState::Working,
        "idle" => ActivityState::Idle,
        "blocked_on_permission" => ActivityState::BlockedOnPermission,
        "errored" => ActivityState::Errored,
        "done" => ActivityState::Done,
        _ => ActivityState::Unknown,
    }
}

#[cfg(test)]
#[path = "classifier_tests.rs"]
mod tests;
