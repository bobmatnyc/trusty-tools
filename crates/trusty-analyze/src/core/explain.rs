//! LLM-backed deep analysis pass: turns a deterministic [`ReviewReport`] into a
//! prose narrative plus framework-aware recommendations.
//!
//! Why: the deterministic review pipeline produces structured data (grades,
//! smells, complexity numbers) that a reviewer still has to interpret. An LLM
//! "explain" pass adds the natural-language synthesis a human reviewer wants:
//! what the change is doing, why the worst files are worst, and which
//! framework-specific concerns apply. Keeping the LLM call *out of* the
//! deterministic [`crate::core::review`] pipeline preserves the `ReviewReport`
//! as a clean, reproducible artifact — the narrative lives on a separate
//! [`DeepAnalysisReport`] so callers can opt into the slower, non-deterministic
//! path without changing the existing review contract.
//!
//! What: [`explain_report`] takes a [`ReviewReport`] + a list of detected
//! framework strings + a [`ChatProvider`], builds a grounded prompt, drains the
//! provider's streaming response into a `String`, and returns the prose
//! narrative. [`deep_analysis`] wraps that into a full [`DeepAnalysisReport`]
//! (narrative + frameworks + recommendations + model id + the underlying
//! deterministic report).
//!
//! Test: see `mod tests` — covers prompt construction, streaming accumulation
//! against a stub provider, the stream-error path, and JSON round-tripping of
//! [`DeepAnalysisReport`].

use serde::{Deserialize, Serialize};
use trusty_common::chat::{ChatEvent, ChatProvider, ToolDef};
use trusty_common::ChatMessage;

use crate::core::review::ReviewReport;
use crate::types::complexity::ComplexityGrade;

/// Default OpenRouter model used by [`deep_analysis`] when the caller doesn't
/// override and `TRUSTY_LLM_MODEL` is unset.
pub const DEFAULT_MODEL: &str = "openai/gpt-4o-mini";

/// Env var holding the OpenRouter API key.
pub const ENV_API_KEY: &str = "OPENROUTER_API_KEY";

/// Env var overriding the default model.
pub const ENV_MODEL: &str = "TRUSTY_LLM_MODEL";

/// Cap on the number of files / smells / recommendations we list in the prompt.
/// Keeps the prompt under ~1500 tokens on a typical fast model so the call
/// stays cheap.
const MAX_FILES_IN_PROMPT: usize = 3;
const MAX_SMELLS_IN_PROMPT: usize = 5;
const MAX_RECS_IN_PROMPT: usize = 5;

/// LLM-augmented deep analysis report.
///
/// Why: keeps the LLM-generated narrative separate from the deterministic
/// [`ReviewReport`] so the two artifacts can be cached, transported, and
/// reasoned about independently. The deterministic report stays a clean
/// fixed-point input; the narrative + framework-aware recommendations are
/// non-deterministic LLM outputs that live on their own wrapper struct.
/// What: `index_id` echoes the request; `narrative` is the LLM prose;
/// `frameworks` is the list passed in (echoed for traceability);
/// `recommendations` is the (best-effort) parsed list of LLM follow-ups;
/// `model_used` is the actual model id; `based_on` is the deterministic input.
/// Test: `deep_report_round_trips_json` confirms the serde shape.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeepAnalysisReport {
    pub index_id: String,
    pub narrative: String,
    pub frameworks: Vec<String>,
    pub recommendations: Vec<String>,
    pub model_used: String,
    pub based_on: ReviewReport,
}

/// Errors returned by [`deep_analysis`] / [`explain_report`].
///
/// Why: keeps the failure surface typed so callers (CLI / HTTP / MCP) can
/// distinguish configuration problems (missing API key or AWS credentials)
/// from runtime ones (chat transport failure). The chat-stream surface itself
/// is anyhow-friendly inside [`explain_report`]; this enum wraps the
/// orchestrator entry point.
/// Test: `missing_api_key_returns_typed_error`, `bedrock_prefix_routing`.
#[derive(Debug, thiserror::Error)]
pub enum DeepAnalysisError {
    /// `OPENROUTER_API_KEY` is not set and no key was passed explicitly (OpenRouter path only).
    #[error("OPENROUTER_API_KEY is not set; deep analysis requires an OpenRouter API key")]
    MissingApiKey,
    /// AWS credentials are not configured for the Bedrock deep-analysis path.
    #[error(
        "AWS credentials not configured for Bedrock deep analysis — \
         set AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY (or AWS_PROFILE, IAM role, SSO)"
    )]
    BedrockAuth,
    /// The LLM provider chat call failed (network, auth, rate limit, etc.).
    #[error("LLM provider chat failed: {0}")]
    Chat(String),
}

/// Generate a prose explanation of `report` using `provider`.
///
/// Why: turns a structured `ReviewReport` into a paragraph a human can read
/// and act on. Calls out the highest-leverage smells, ties them to the worst
/// files, and (when `frameworks` is non-empty) frames the advice in terms of
/// the project's detected frameworks. Taking the provider as a `dyn` lets
/// tests inject a stub without hitting the network.
/// What: builds a grounded prompt with [`build_explain_prompt`], sends it as a
/// single `user` message via the provider's streaming chat API, and
/// accumulates `Delta` events into a `String`. Returns an error if the
/// provider returns an error event or its stream task fails.
/// Test: `explain_report_collects_stream_deltas` and
/// `explain_report_surfaces_stream_error`.
pub async fn explain_report(
    report: &ReviewReport,
    frameworks: &[String],
    provider: &dyn ChatProvider,
) -> anyhow::Result<String> {
    let prompt = build_explain_prompt(report, frameworks);
    let messages = vec![
        ChatMessage {
            role: "system".to_string(),
            content: SYSTEM_PROMPT.to_string(),
            tool_call_id: None,
            tool_calls: None,
        },
        ChatMessage {
            role: "user".to_string(),
            content: prompt,
            tool_call_id: None,
            tool_calls: None,
        },
    ];

    // Buffered channel keeps the provider task from blocking on a slow reader.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<ChatEvent>(32);
    let stream_fut = provider.chat_stream(messages, Vec::<ToolDef>::new(), tx);

    let drain = async {
        let mut out = String::new();
        let mut stream_error: Option<String> = None;
        while let Some(event) = rx.recv().await {
            match event {
                ChatEvent::Delta(s) => out.push_str(&s),
                ChatEvent::ToolCall(_) => {
                    // Tools aren't used for explain; ignore spurious calls.
                }
                ChatEvent::Done => break,
                ChatEvent::Error(msg) => {
                    stream_error = Some(msg);
                    break;
                }
            }
        }
        if let Some(msg) = stream_error {
            anyhow::bail!("chat provider stream error: {msg}");
        }
        Ok::<String, anyhow::Error>(out)
    };

    let (stream_res, narrative) = tokio::join!(stream_fut, drain);
    stream_res?;
    narrative
}

/// System prompt used for every explain call.
const SYSTEM_PROMPT: &str = "You are a code review assistant. Given structured \
metrics from a pull-request review (complexity grade, code smells, recommendations, \
and detected frameworks), explain the findings in 2-3 short paragraphs of plain prose. \
Focus on why these issues matter and the highest-priority next steps the developer \
should take. Be specific: reference the file paths, smell categories, and frameworks \
provided. Do not invent metrics that aren't in the input.";

/// Build the user-message prompt body from `report` and `frameworks`.
///
/// Why: deterministic prompt construction keeps the LLM output grounded in the
/// exact metrics from the report and isolates the network-free part of the
/// pipeline so it can be unit-tested without a real provider.
/// What: assembles a text block listing overall grade, smell count,
/// changed-line count, detected frameworks, the worst [`MAX_FILES_IN_PROMPT`]
/// files (worst first), the first [`MAX_SMELLS_IN_PROMPT`] smells across the
/// report, and the first [`MAX_RECS_IN_PROMPT`] static recommendations.
/// Test: `build_explain_prompt_*` tests.
pub fn build_explain_prompt(report: &ReviewReport, frameworks: &[String]) -> String {
    let mut out = String::new();

    out.push_str(&format!(
        "Overall grade: {}\nSmell count: {}\nChanged lines: {}\nFiles reviewed: {}\n",
        report.overall_grade,
        report.smell_count,
        report.changed_lines,
        report.files.len(),
    ));

    if !frameworks.is_empty() {
        out.push_str(&format!("Detected frameworks: {}\n", frameworks.join(", ")));
    }

    out.push('\n');
    out.push_str(&format!("Summary: {}\n\n", report.summary));

    // Worst files (highest grade enum value wins). Stable sort preserves the
    // original ordering for files with the same grade.
    let mut worst: Vec<&_> = report.files.iter().collect();
    worst.sort_by_key(|b| std::cmp::Reverse(b.grade));
    worst.truncate(MAX_FILES_IN_PROMPT);

    if !worst.is_empty() {
        out.push_str("Worst files (worst first):\n");
        for f in &worst {
            out.push_str(&format!(
                "- {} (grade {}, cyclomatic {}, cognitive {}, {} smell(s))\n",
                f.path,
                f.grade,
                f.complexity.cyclomatic,
                f.complexity.cognitive,
                f.smells.len(),
            ));
        }
        out.push('\n');
    }

    // Top smells across all files.
    let mut smells: Vec<(&str, &_)> = Vec::new();
    for f in &report.files {
        for s in &f.smells {
            smells.push((f.path.as_str(), s));
            if smells.len() >= MAX_SMELLS_IN_PROMPT {
                break;
            }
        }
        if smells.len() >= MAX_SMELLS_IN_PROMPT {
            break;
        }
    }
    if !smells.is_empty() {
        out.push_str("Top smells:\n");
        for (path, s) in &smells {
            out.push_str(&format!(
                "- {} at {}:{} (severity {})\n",
                s.category, path, s.line, s.severity,
            ));
        }
        out.push('\n');
    }

    // Feed existing static recommendations so the LLM expands on them rather
    // than reinventing them.
    let recs: Vec<&str> = report
        .files
        .iter()
        .flat_map(|f| f.recommendations.iter().map(String::as_str))
        .take(MAX_RECS_IN_PROMPT)
        .collect();
    if !recs.is_empty() {
        out.push_str("Existing static recommendations:\n");
        for r in &recs {
            out.push_str(&format!("- {r}\n"));
        }
        out.push('\n');
    }

    out.push_str(
        "Write 2-3 short paragraphs explaining why these findings matter and the prioritised \
         next steps for the developer. Reference specific files, smells, and (where relevant) \
         the detected frameworks.",
    );

    // Hint: avoid an empty grade-A report producing a confusing prompt.
    if report.files.is_empty() && report.overall_grade == ComplexityGrade::A {
        out.push_str(
            "\n\nNote: this diff is empty or has no measurable findings; \
             explain briefly that no review-worthy issues were detected.",
        );
    }

    out
}

/// Extract recommendation bullet points from an LLM prose narrative.
///
/// Why: the LLM is asked for prose; downstream consumers (CLI, dashboards)
/// often also want a separate "what to do" list. Rather than ask the model for
/// strict JSON (and then deal with malformed responses), we mine the narrative
/// itself for any bullet-list lines the model emitted. This is a best-effort
/// projection — empty when the narrative is pure prose with no bullets.
/// What: scans lines, trims a leading `-`, `*`, or `1.`-style marker, and
/// keeps every non-empty line that had a marker.
/// Test: `extract_recommendations_picks_bullets`.
fn extract_recommendations(narrative: &str) -> Vec<String> {
    let mut out = Vec::new();
    for raw in narrative.lines() {
        let line = raw.trim();
        if let Some(rest) = strip_bullet_marker(line) {
            let cleaned = rest.trim();
            if !cleaned.is_empty() {
                out.push(cleaned.to_string());
            }
        }
    }
    out
}

/// Strip a leading bullet marker (`-`, `*`, `•`, or `N.`) from `line` if
/// present. Returns the remainder, or `None` if the line is unmarked.
fn strip_bullet_marker(line: &str) -> Option<&str> {
    for marker in ["- ", "* ", "• "] {
        if let Some(rest) = line.strip_prefix(marker) {
            return Some(rest);
        }
    }
    // `1.` / `12.` style: split on first '.' if the prefix is all digits.
    if let Some(dot) = line.find('.') {
        let head = &line[..dot];
        if !head.is_empty() && head.chars().all(|c| c.is_ascii_digit()) {
            // require a space after the dot
            let rest = &line[dot + 1..];
            if let Some(stripped) = rest.strip_prefix(' ') {
                return Some(stripped);
            }
        }
    }
    None
}

/// Read `OPENROUTER_API_KEY` from the env, preferring an explicit override.
///
/// Why: the binary layer typically reads the env var once at startup and
/// threads it through; tests may want to pass a key explicitly without
/// touching the environment.
/// What: returns the explicit key when non-empty, then the env var, then
/// [`DeepAnalysisError::MissingApiKey`].
/// Test: covered transitively by `missing_api_key_returns_typed_error`.
pub fn resolve_api_key(explicit: Option<&str>) -> Result<String, DeepAnalysisError> {
    if let Some(k) = explicit.filter(|s| !s.is_empty()) {
        return Ok(k.to_string());
    }
    std::env::var(ENV_API_KEY)
        .ok()
        .filter(|s| !s.is_empty())
        .ok_or(DeepAnalysisError::MissingApiKey)
}

/// Resolve the model id from explicit > [`ENV_MODEL`] > [`DEFAULT_MODEL`].
pub fn resolve_model(explicit: Option<&str>) -> String {
    if let Some(m) = explicit.filter(|s| !s.is_empty()) {
        return m.to_string();
    }
    std::env::var(ENV_MODEL)
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_MODEL.to_string())
}

/// Classify an anyhow error from the Bedrock chat path into a typed
/// [`DeepAnalysisError`].
///
/// Why: the string-heuristic that promotes credential-related errors to
/// [`DeepAnalysisError::BedrockAuth`] lives in one testable place instead of
/// being inlined inside the async orchestrator. Extracting it prevents a
/// refactor of upstream error wording from silently demoting auth failures to
/// the generic [`DeepAnalysisError::Chat`] variant.
/// What: returns [`DeepAnalysisError::BedrockAuth`] when the lower-cased
/// message contains `"credential"`, `"no credentials"`, or `"aws"`; falls
/// back to [`DeepAnalysisError::Chat`] otherwise.
/// Test: `bedrock_credential_error_classifies_as_bedrock_auth` and
/// `non_credential_bedrock_error_classifies_as_chat`.
pub(crate) fn classify_bedrock_error(e: &anyhow::Error) -> DeepAnalysisError {
    let msg = format!("{e:#}");
    let lower = msg.to_lowercase();
    if lower.contains("credential") || lower.contains("no credentials") || lower.contains("aws") {
        DeepAnalysisError::BedrockAuth
    } else {
        DeepAnalysisError::Chat(msg)
    }
}

/// Run the Bedrock deep-analysis path with an explicitly supplied provider.
///
/// Why: allows tests (and future callers that already hold a provider) to
/// exercise the full Bedrock narrative + error-mapping pipeline without going
/// through `BedrockProvider::new`, which requires live AWS credentials.
/// What: calls [`explain_report`], maps credential-related errors to
/// [`DeepAnalysisError::BedrockAuth`] via [`classify_bedrock_error`], and on
/// success extracts recommendations and returns a [`DeepAnalysisReport`].
/// Test: `bedrock_credential_error_maps_to_bedrock_auth` injects a stub
/// provider whose stream error contains "credential" and asserts
/// `BedrockAuth` is returned.
pub(crate) async fn explain_with_bedrock_provider(
    index_id: &str,
    report: ReviewReport,
    frameworks: Vec<String>,
    model: &str,
    provider: &dyn ChatProvider,
) -> Result<DeepAnalysisReport, DeepAnalysisError> {
    let narrative = explain_report(&report, &frameworks, provider)
        .await
        .map_err(|e| classify_bedrock_error(&e))?;

    let recommendations = extract_recommendations(&narrative);

    Ok(DeepAnalysisReport {
        index_id: index_id.to_string(),
        narrative,
        frameworks,
        recommendations,
        model_used: model.to_string(),
        based_on: report,
    })
}

/// Prefix that selects the Bedrock provider in `TRUSTY_LLM_MODEL`.
///
/// Set `TRUSTY_LLM_MODEL=bedrock/<bedrock-model-id>` to route the deep pass
/// through AWS Bedrock. Omitting the prefix (or using any other prefix) routes
/// to OpenRouter.
pub const BEDROCK_MODEL_PREFIX: &str = "bedrock/";

/// Default Bedrock model id used when the caller sets
/// `TRUSTY_LLM_MODEL=bedrock/` without a trailing model id.
///
/// Resolves to `us.anthropic.claude-sonnet-4-6` — the Claude Sonnet 4.6
/// cross-region inference profile (no date stamp or `-v1:0` suffix; verified
/// against AWS docs). Delegates to [`trusty_common::chat::DEFAULT_BEDROCK_MODEL`].
pub const DEFAULT_BEDROCK_MODEL_ID: &str = trusty_common::chat::DEFAULT_BEDROCK_MODEL;

/// Run a full deep-analysis pass, routing to AWS Bedrock or OpenRouter based
/// on the model id prefix.
///
/// Why: the single orchestration entry point used by the HTTP, MCP, and CLI
/// layers so they all produce identical [`DeepAnalysisReport`]s regardless of
/// transport. Keeping the provider construction in one place means env-var
/// resolution and model-default behaviour live in exactly one location.
/// What: resolves the model id (env or override). If the model id starts with
/// `bedrock/`, constructs a [`trusty_common::chat::BedrockProvider`] using the
/// standard AWS credential chain (no API key needed). Otherwise, resolves the
/// OpenRouter API key and constructs an
/// [`trusty_common::chat::OpenRouterProvider`]. Delegates to
/// [`explain_with_bedrock_provider`] for the Bedrock path (enabling injection
/// in tests); runs inline for the OpenRouter path.
/// Test: `missing_api_key_returns_typed_error` (OpenRouter no-key path);
/// `bedrock_prefix_routing` (routing unit test, no network).
pub async fn deep_analysis(
    index_id: &str,
    report: ReviewReport,
    frameworks: Vec<String>,
    api_key: Option<&str>,
    model: Option<&str>,
) -> Result<DeepAnalysisReport, DeepAnalysisError> {
    let model = resolve_model(model);

    if model.starts_with(BEDROCK_MODEL_PREFIX) {
        // ── Bedrock path ──────────────────────────────────────────────────
        let bedrock_model_id = model
            .strip_prefix(BEDROCK_MODEL_PREFIX)
            .filter(|s| !s.is_empty())
            .unwrap_or(DEFAULT_BEDROCK_MODEL_ID);

        let region = std::env::var("TRUSTY_AWS_REGION")
            .or_else(|_| std::env::var("AWS_REGION"))
            .ok();

        use trusty_common::chat::BedrockProvider;
        let provider = BedrockProvider::new(bedrock_model_id, region.as_deref())
            .await
            .map_err(|e| DeepAnalysisError::Chat(format!("Bedrock provider init: {e:#}")))?;

        explain_with_bedrock_provider(index_id, report, frameworks, &model, &provider).await
    } else {
        // ── OpenRouter path ───────────────────────────────────────────────
        use trusty_common::chat::OpenRouterProvider;

        let api_key = resolve_api_key(api_key)?;
        let provider = OpenRouterProvider::new(api_key, &model);

        let narrative = explain_report(&report, &frameworks, &provider)
            .await
            .map_err(|e| DeepAnalysisError::Chat(format!("{e:#}")))?;

        let recommendations = extract_recommendations(&narrative);

        Ok(DeepAnalysisReport {
            index_id: index_id.to_string(),
            narrative,
            frameworks,
            recommendations,
            model_used: model,
            based_on: report,
        })
    }
}

/// Render a [`DeepAnalysisReport`] as a human-readable text block.
///
/// Why: the CLI `deep --format text` mode wants something a person can scan in
/// a terminal, parallel to [`crate::core::render_review_text`].
/// What: emits the index id, model, frameworks, narrative, parsed
/// recommendations, and a one-line summary of the underlying deterministic
/// report.
/// Test: `render_text_includes_narrative_and_frameworks`.
pub fn render_text(report: &DeepAnalysisReport) -> String {
    let mut out = String::new();
    out.push_str("=== Deep Analysis ===\n");
    out.push_str(&format!("index: {}\n", report.index_id));
    out.push_str(&format!("model: {}\n", report.model_used));
    if report.frameworks.is_empty() {
        out.push_str("frameworks: none detected\n");
    } else {
        out.push_str(&format!("frameworks: {}\n", report.frameworks.join(", ")));
    }
    out.push_str("\n--- Narrative ---\n");
    out.push_str(report.narrative.trim());
    out.push('\n');
    if !report.recommendations.is_empty() {
        out.push_str("\n--- Recommendations ---\n");
        for r in &report.recommendations {
            out.push_str(&format!("  - {r}\n"));
        }
    }
    out.push_str(&format!(
        "\n--- Based On ---\n{}\n",
        report.based_on.summary
    ));
    out
}

#[cfg(test)]
#[path = "explain_tests.rs"]
mod tests;
