//! Project in-process state into a `PmResponse` envelope (#151 Phase 1).
//!
//! Why: The workflow engine already collects everything callers need —
//! phase outputs, summaries, token usage, cost, status. We just need a
//! single place that reshapes it into the stable `PmResponse` JSON.
//! What: `build_from_workflow` takes the final `WorkflowContext`,
//! `PerfRecord`, wall-clock duration, and optional narrative override
//! and returns a populated `PmResponse`.
//! Test: `build_from_workflow_populates_phases_and_metadata`.

use std::path::Path;

use crate::api::types::{
    PhaseResult, PmMetadata, PmResponse, PmResponseType, PmStatus, now_iso8601,
};
use crate::perf::PerfRecord;
use crate::workflow::WorkflowContext;

/// Build a `PmResponse` from a completed workflow run.
pub fn build_from_workflow(
    ctx: &WorkflowContext,
    perf: Option<&PerfRecord>,
    narrative: String,
    workflow_name: Option<&str>,
    errors: Vec<String>,
) -> PmResponse {
    let status = match perf.map(|p| p.status.as_str()) {
        Some("success") => PmStatus::Success,
        Some("partial") => PmStatus::Partial,
        Some("failed") => PmStatus::Failed,
        _ if !errors.is_empty() => PmStatus::Failed,
        _ => PmStatus::Success,
    };

    let mut metadata = PmMetadata {
        workflow: workflow_name.map(|s| s.to_string()),
        ..Default::default()
    };
    let mut phases: Vec<PhaseResult> = Vec::new();

    if let Some(p) = perf {
        metadata.processing_time_ms = p.total_duration_ms;
        metadata.total_tokens_in = p.totals.prompt_tokens as u64;
        metadata.total_tokens_out = p.totals.completion_tokens as u64;
        metadata.cache_read_tokens = p.totals.cache_read_tokens as u64;
        metadata.cache_creation_tokens = p.totals.cache_creation_tokens as u64;
        metadata.total_cost_usd = p.totals.cost_usd;
        metadata.build_number = Some(p.build);
        metadata.workflow = metadata.workflow.or_else(|| Some(p.workflow.clone()));

        for phase in &p.phases {
            let summary = ctx
                .phase_summaries
                .get(&phase.name)
                .cloned()
                .or_else(|| ctx.phase_outputs.get(&phase.name).cloned())
                .unwrap_or_default();
            phases.push(PhaseResult {
                name: phase.name.clone(),
                status: PmStatus::Success, // perf only stores aggregate status
                duration_ms: phase.duration_ms,
                tokens_in: phase.prompt_tokens as u64,
                tokens_out: phase.completion_tokens as u64,
                summary,
            });
        }
    }

    let out_dir_str = ctx.out_dir.as_ref().map(|p| p.display().to_string());

    let files_modified = ctx
        .out_dir
        .as_ref()
        .map(|d| list_files_in_dir(d))
        .unwrap_or_default();

    // #149: Project the per-phase perf records into a `phases_completed`
    // timeline so JSON envelope consumers (the Tauri UI poller, CLI tooling)
    // see the same live progress that streamed to the terminal.
    let phases_completed = perf
        .map(|p| {
            p.phases
                .iter()
                .map(|ph| crate::api::types::PhaseProgress {
                    name: ph.name.clone(),
                    status: "done".to_string(),
                    elapsed_secs: ph.duration_ms as f32 / 1000.0,
                    cost_usd: ph.cost_usd as f32,
                    note: None,
                })
                .collect()
        })
        .unwrap_or_default();

    PmResponse {
        id: uuid::Uuid::new_v4().to_string(),
        timestamp: now_iso8601(),
        response_type: PmResponseType::WorkflowResult,
        status,
        narrative,
        metadata,
        phases,
        files_modified,
        out_dir: out_dir_str,
        errors,
        phases_completed,
    }
}

/// Recursively list relative paths of files under `dir`.
///
/// Why: `files_modified` in `PmResponse` surfaces what the workflow wrote to
/// disk; walking `out_dir` after the run is a cheap proxy that works for
/// every workflow regardless of whether it emitted files via NDJSON
/// extraction or direct tool writes.
/// What: Walks `dir` (skipping hidden entries), returns relative paths as
/// forward-slash strings. Returns empty list on any I/O error.
/// Test: `list_files_handles_missing_dir`.
fn list_files_in_dir(dir: &Path) -> Vec<String> {
    if !dir.exists() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let walker = walkdir::WalkDir::new(dir)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            // Skip dotfiles/dirs (.git, .open-mpm, etc.)
            e.file_name()
                .to_str()
                .map(|s| !s.starts_with('.') || e.depth() == 0)
                .unwrap_or(true)
        });
    for entry in walker.flatten() {
        if entry.file_type().is_file()
            && let Ok(rel) = entry.path().strip_prefix(dir)
        {
            out.push(rel.to_string_lossy().replace('\\', "/"));
        }
    }
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn list_files_handles_missing_dir() {
        let v = list_files_in_dir(Path::new("/nonexistent/path/for/test/xyz"));
        assert!(v.is_empty());
    }

    #[test]
    fn build_from_workflow_populates_phases_and_metadata() {
        let mut ctx = WorkflowContext::builder("do x")
            .with_out_dir(Some(PathBuf::from("/tmp/does-not-exist-xyz")))
            .build();
        ctx.record_phase("research", "full".into(), Some("sum".into()));
        let perf = PerfRecord {
            build: 42,
            version: "0.1.0".into(),
            workflow: "test-wf".into(),
            task_preview: "do x".into(),
            started_at: "2026-01-01T00:00:00Z".into(),
            total_duration_ms: 123,
            phases: vec![crate::perf::PhaseRecord {
                name: "research".into(),
                duration_ms: 50,
                prompt_tokens: 100,
                completion_tokens: 200,
                cache_read_tokens: 0,
                cache_creation_tokens: 0,
                cost_usd: 0.01,
            }],
            totals: crate::perf::PerfTotals {
                prompt_tokens: 100,
                completion_tokens: 200,
                cache_read_tokens: 0,
                cache_creation_tokens: 0,
                cost_usd: 0.01,
            },
            status: "success".into(),
            failed_phase: None,
            skills_used: Vec::new(),
            skills_considered: Vec::new(),
            tests_passed: None,
            tests_failed: None,
        };
        let resp = build_from_workflow(&ctx, Some(&perf), "ok".into(), Some("test-wf"), vec![]);
        assert_eq!(resp.status, PmStatus::Success);
        assert_eq!(resp.phases.len(), 1);
        assert_eq!(resp.phases[0].name, "research");
        assert_eq!(resp.phases[0].summary, "sum");
        assert_eq!(resp.metadata.total_tokens_in, 100);
        assert_eq!(resp.metadata.total_tokens_out, 200);
        assert_eq!(resp.metadata.build_number, Some(42));
        assert_eq!(resp.narrative, "ok");
    }
}
