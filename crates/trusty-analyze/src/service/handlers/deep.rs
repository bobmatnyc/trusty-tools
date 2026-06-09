//! Route handlers for deep LLM analysis and helper synthesis functions.
//!
//! Why: Extracted from `handlers/review.rs` to keep both files under the
//! 500-line cap. This module owns the "LLM narrative pass" surface: the
//! `POST /analyze/deep` endpoint, its request struct, the private corpus
//! synthesiser, and the framework-lookup helper shared with that handler.
//!
//! What: Three exported items:
//! - `DeepAnalyzeRequest` — JSON body struct for `POST /analyze/deep`
//! - `deep_analyze_handler` — axum handler that calls the LLM deep-analysis core
//! - `synthesise_review_from_chunks` — pure aggregation fn (unit-testable)
//! - `lookup_frameworks` — reads framework facts from FactStore
//!
//! Test: `deep_endpoint_requires_index_id`, `deep_endpoint_requires_api_key`,
//! `synthesise_review_from_chunks_groups_by_file`,
//! `synthesise_review_from_chunks_empty_corpus_is_grade_a`,
//! `lookup_frameworks_reads_stored_facts` (all in `service/tests_review.rs`).

use std::sync::Arc;

use axum::{extract::State, response::Json};
use serde::Deserialize;

use crate::service::events::{AnalyzerAppState, ApiError};

/// Request body for `POST /analyze/deep`.
///
/// Why: deep analysis is opt-in and parameterised, so the endpoint takes a
/// JSON body rather than a query string. Callers either pass a pre-computed
/// [`crate::core::ReviewReport`] (to avoid the re-review cost) or omit it,
/// in which case the endpoint synthesises a report by aggregating the index's
/// chunk corpus with the same complexity / smell math used by `/review`.
/// What: `index_id` is required; `report` is optional; `model` overrides the
/// daemon-default LLM model.
/// Test: `deep_endpoint_requires_index_id` covers the missing-field 400 path;
/// `deep_endpoint_requires_api_key` covers the no-key 400 path.
#[derive(Debug, Deserialize)]
pub struct DeepAnalyzeRequest {
    pub index_id: String,
    #[serde(default)]
    pub report: Option<crate::core::ReviewReport>,
    #[serde(default)]
    pub model: Option<String>,
}

/// Why: turns a deterministic [`crate::core::ReviewReport`] into a
/// [`crate::core::DeepAnalysisReport`] by running an OpenRouter chat call. The
/// LLM pass is deliberately separated from `/review` so the deterministic
/// surface stays cheap, reproducible, and free of network/AI dependencies.
/// What: requires `index_id` in the JSON body; either uses the provided
/// `report` or builds one from the index's chunk corpus (no diff: the
/// synthesised report treats the whole indexed corpus as one big "file" set
/// for grading purposes). Reads frameworks from the analyzer's `FactStore`
/// (predicate `"uses_framework"`), calls `deep_analysis`, and returns the
/// wrapper report. Requires `OPENROUTER_API_KEY` to be configured at startup
/// — returns 400 with `MissingApiKey` otherwise.
/// Test: `deep_endpoint_requires_api_key`, `deep_endpoint_requires_index_id`.
pub async fn deep_analyze_handler(
    State(state): State<Arc<AnalyzerAppState>>,
    Json(req): Json<DeepAnalyzeRequest>,
) -> Result<Json<crate::core::DeepAnalysisReport>, ApiError> {
    if req.index_id.trim().is_empty() {
        return Err(ApiError::bad_request("missing required 'index_id' field"));
    }

    // Determine the effective model id so we can decide whether an API key is
    // required. Bedrock models (prefixed with "bedrock/") use AWS credential
    // chain auth — no OPENROUTER_API_KEY needed.
    let effective_model = req
        .model
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or(&state.llm_model);

    let uses_bedrock = effective_model.starts_with(crate::core::explain::BEDROCK_MODEL_PREFIX);

    let api_key = if uses_bedrock {
        // Bedrock path: no OpenRouter key needed.
        None
    } else {
        let key = state.api_key.as_deref().filter(|s| !s.is_empty());
        if key.is_none() {
            return Err(ApiError::bad_request(
                "OPENROUTER_API_KEY is not configured on the daemon; \
                 set OPENROUTER_API_KEY in the environment and restart the daemon, \
                 or use a bedrock/<model-id> model instead",
            ));
        }
        key
    };

    // Either use the caller-supplied report, or synthesise one from the index
    // corpus. Synthesis: treat the whole indexed corpus as one big "no-diff"
    // review by running the deterministic complexity/smell math over every
    // chunk and rolling up per-file metrics. This keeps the LLM input shaped
    // identically to the diff-based path.
    let report = match req.report {
        Some(r) => r,
        None => synthesise_review_from_index(&state, &req.index_id).await?,
    };

    // Pull detected frameworks from the FactStore (recorded by `record_frameworks`).
    let frameworks = lookup_frameworks(&state, &req.index_id);

    let model_override = req.model.as_deref();
    let report = crate::core::deep_analysis(
        &req.index_id,
        report,
        frameworks,
        api_key,
        model_override.or(Some(&state.llm_model)),
    )
    .await
    .map_err(|e| match e {
        crate::core::DeepAnalysisError::MissingApiKey => ApiError::bad_request(format!("{e}")),
        crate::core::DeepAnalysisError::BedrockAuth => ApiError::bad_request(format!("{e}")),
        crate::core::DeepAnalysisError::Chat(_) => ApiError::bad_gateway(format!("{e}")),
    })?;
    Ok(Json(report))
}

/// Build a [`crate::core::ReviewReport`] from an index's chunk corpus without
/// any diff input.
///
/// Why: `POST /analyze/deep` accepts an optional `report` field — when the
/// caller omits it, we still need a deterministic report shape to feed the
/// LLM. Synthesising one from the indexed corpus gives the LLM the same
/// metrics it would see for a diff that touched every file in the index.
/// What: fetches the corpus, groups chunks by file, computes per-file
/// complexity / smells / grade, and aggregates them into a [`ReviewReport`]
/// with `source = NewFile` (since we have no diff to anchor "modified chunks"
/// against).
/// Test: covered indirectly by `deep_endpoint_requires_api_key` (the synth
/// step succeeds against the stub search; the 400 then comes from the key
/// guard). A unit test covers `synthesise_review_from_chunks` directly.
async fn synthesise_review_from_index(
    state: &AnalyzerAppState,
    index_id: &str,
) -> Result<crate::core::ReviewReport, ApiError> {
    let chunks = state.search.get_chunks(index_id).await.map_err(|e| {
        ApiError::bad_gateway(format!("get_chunks({index_id}) for deep analysis: {e:#}"))
    })?;
    Ok(synthesise_review_from_chunks(&chunks))
}

/// Pure helper: aggregate a chunk corpus into a [`crate::core::ReviewReport`].
///
/// Why: extracted into a free function so it can be unit-tested without an
/// HTTP client.
/// What: groups chunks by file path, runs `compute_complexity_for` + smell
/// detection per file, builds `FileReview`s with `ReviewSource::NewFile`,
/// rolls up the worst grade and total smell count.
/// Test: `synthesise_review_from_chunks_groups_by_file`.
pub(crate) fn synthesise_review_from_chunks(
    chunks: &[crate::types::CodeChunk],
) -> crate::core::ReviewReport {
    use crate::core::complexity::{compute_complexity_for, detect_smells};
    use crate::core::review::{FileReview, ReviewComplexity, ReviewSource, SmellHit};
    use crate::types::complexity::CodeSmell;
    use std::collections::BTreeMap;

    // Snake_case projection for code smells. Mirrors review.rs's
    // smell_projection, kept local to avoid widening the review.rs public
    // surface for this synth-only consumer.
    fn project(s: &CodeSmell) -> (&'static str, &'static str) {
        match s {
            CodeSmell::LongFunction { .. } => ("long_method", "medium"),
            CodeSmell::DeepNesting { .. } => ("deep_nesting", "high"),
            CodeSmell::TooManyParams { .. } => ("too_many_params", "medium"),
            CodeSmell::MissingDocstring => ("missing_docstring", "low"),
        }
    }

    let mut by_file: BTreeMap<String, Vec<&crate::types::CodeChunk>> = BTreeMap::new();
    for c in chunks {
        by_file.entry(c.file.clone()).or_default().push(c);
    }

    let mut files: Vec<FileReview> = Vec::with_capacity(by_file.len());
    let mut total_smells = 0usize;
    let mut total_lines = 0usize;
    for (path, group) in by_file {
        let joined: String = group
            .iter()
            .map(|c| c.content.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let lang = match path.rsplit('.').next().unwrap_or("") {
            "rs" => "rust",
            "ts" => "typescript",
            "tsx" => "tsx",
            "js" => "javascript",
            "jsx" => "jsx",
            "py" => "python",
            "go" => "go",
            "java" => "java",
            _ => "unknown",
        };
        let metrics = compute_complexity_for(&joined, lang);
        let raw_smells = detect_smells(&joined);
        let smells: Vec<SmellHit> = raw_smells
            .iter()
            .map(|s| {
                let (category, severity) = project(s);
                SmellHit {
                    category: category.to_string(),
                    line: group.first().map(|c| c.start_line as u32).unwrap_or(0),
                    severity: severity.to_string(),
                }
            })
            .collect();
        total_smells += smells.len();
        total_lines += joined.lines().count();
        files.push(FileReview {
            path,
            grade: metrics.grade,
            complexity: ReviewComplexity {
                cyclomatic: metrics.cyclomatic,
                cognitive: metrics.cognitive,
            },
            smells,
            recommendations: Vec::new(),
            source: ReviewSource::NewFile,
        });
    }

    let overall_grade = files
        .iter()
        .map(|f| f.grade)
        .max()
        .unwrap_or(crate::types::ComplexityGrade::A);
    let summary = format!(
        "{} file(s) synthesised from index corpus; {} smell(s); overall grade {}",
        files.len(),
        total_smells,
        overall_grade
    );

    crate::core::ReviewReport {
        files,
        overall_grade,
        changed_lines: total_lines,
        smell_count: total_smells,
        summary,
    }
}

/// Look up framework names recorded for `index_id` in the FactStore.
///
/// Why: framework detection runs as a separate setup step
/// (`record_frameworks`) and persists results as `(index_id, "uses_framework",
/// <name>)` triples. The deep-analysis path reads them back here so the LLM
/// prompt is framework-aware without having to re-scan the filesystem.
/// What: queries facts with `predicate = "uses_framework"` filtered by
/// `index_id` (via the `subject` column which the recorder uses as the index
/// id key), returning the deduplicated, sorted list of object values.
/// Test: covered transitively by the `deep_endpoint_*` tests; failures fall
/// back to an empty list rather than hard-erroring.
pub(crate) fn lookup_frameworks(state: &AnalyzerAppState, index_id: &str) -> Vec<String> {
    use std::collections::BTreeSet;
    let Ok(hits) = state
        .facts
        .query(Some(index_id), Some("uses_framework"), None)
    else {
        return Vec::new();
    };
    let mut names: BTreeSet<String> = BTreeSet::new();
    for fact in hits {
        names.insert(fact.object);
    }
    names.into_iter().collect()
}
