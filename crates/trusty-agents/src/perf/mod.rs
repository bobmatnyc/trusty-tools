//! Performance instrumentation — per-phase latency + token/cost capture.
//!
//! Why: (#47) We need to track how long each workflow phase takes, how many
//! tokens it consumed (prompt / completion / Anthropic cache read / cache
//! creation), and the resulting USD cost so we can compare runs build-over-
//! build and catch regressions or prompt-caching wins. Persisting one JSON
//! file per run under `docs/performance/runs/` plus a one-line summary log
//! keeps the data greppable without a database.
//! What: `PerfCollector` is constructed at workflow start, `record_phase`
//! appends a `PhaseRecord` after each phase, and `flush` writes the final
//! JSON + log line.
//!
//! The portable plain-data types (`TokenUsage`, `PhaseRecord`, `PerfTotals`,
//! `PerfRecord`) were moved to `trusty-agents-common::perf` in Wave 2
//! (issue #867, refs #830/#832). They are re-exported here so all existing
//! `crate::perf::TokenUsage` etc. references inside `trusty-agents` continue
//! to resolve unchanged.
//!
//! Test: `cost_usd_known_model`, `collector_records_phases`,
//! `collector_totals_sum_phases`, `collector_flush_writes_json_and_log`.
//!
//! Module layout (see #366 split):
//! - `mod.rs` — `PerfCollector` lifecycle + explicit re-exports of the
//!   portable types from `trusty-agents-common::perf`
//! - `pricing.rs` — `cost_usd` + the model pricing table + formatters
//! - `tests.rs` — unit tests

mod pricing;

#[cfg(test)]
mod tests;

use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result};
use chrono::Utc;

use pricing::{filename_stamp, truncate_preview};

pub use pricing::cost_usd;

// Why: `TokenUsage`, `PhaseRecord`, `PerfTotals`, and `PerfRecord` were
//      extracted to `trusty-agents-common::perf` in Wave 2 (issue #867) so
//      external crates and the `AgentRunner` seam can reference `TokenUsage`
//      without depending on the full `trusty-agents` binary crate. Re-exports
//      here preserve every existing `crate::perf::TokenUsage` etc. import
//      in the workspace — internal call sites are unchanged.
// What: Explicit re-exports of all four portable value types.
// Test: All existing perf tests still resolve these names and pass.
pub use trusty_agents_common::perf::{PerfRecord, PerfTotals, PhaseRecord, TokenUsage};

/// Accumulator used during workflow execution.
///
/// Why: Keeping the collector off the workflow context lets us evolve perf
/// schema without churning `WorkflowContext`; the engine just calls
/// `record_phase`/`flush`.
/// What: Holds the build stamp, workflow name, a 120-char preview of the
/// user task, the wall-clock start `Instant`, and a growing `Vec<PhaseRecord>`.
/// `flush(out_dir)` writes `runs/YYYYMMDD-HHMMSS-build<N>.json` and appends a
/// summary line to `runs.log`.
/// Test: see unit tests at bottom of this file.
pub struct PerfCollector {
    build: u64,
    workflow: String,
    task_preview: String,
    started_at: Instant,
    started_at_iso: String,
    phases: Vec<PhaseRecord>,
    model: String,
    /// #56: run status; set via `set_status`. Defaults to "success".
    status: String,
    /// #56: name of the phase that failed, if any.
    failed_phase: Option<String>,
    /// #171: distinct skills injected during the run, insertion-ordered.
    skills_used: Vec<String>,
    /// #173: distinct skills matched by pre-plan discovery, insertion-ordered.
    skills_considered: Vec<String>,
    /// QA-emitted passed test count, when available.
    tests_passed: Option<u64>,
    /// QA-emitted failed test count, when available.
    tests_failed: Option<u64>,
}

impl PerfCollector {
    /// Construct a new collector.
    ///
    /// Why: Called at workflow start so the wall-clock anchor is the true
    /// run start, not a lazy "first phase" moment.
    /// What: Captures the build number, workflow name, truncated task preview,
    /// start `Instant`, and start ISO8601 UTC timestamp.
    /// Test: `collector_records_phases`.
    pub fn new(build: u64, workflow: &str, task: &str) -> Self {
        Self {
            build,
            workflow: workflow.to_string(),
            task_preview: truncate_preview(task, 120),
            started_at: Instant::now(),
            started_at_iso: Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            phases: Vec::new(),
            model: String::new(),
            status: "success".to_string(),
            failed_phase: None,
            skills_used: Vec::new(),
            skills_considered: Vec::new(),
            tests_passed: None,
            tests_failed: None,
        }
    }

    /// Record QA test counts (Fix 3 / parity with claude-mpm).
    ///
    /// Why: When the QA agent emits a parsable JSON envelope, we want the
    /// exact `passed`/`failed` counts on the perf record so dashboards can
    /// chart them over time without re-parsing agent output.
    /// What: Stores the two counts; included in the flushed JSON and the
    /// `runs.log` summary line.
    /// Test: `test_run_record_serializes_test_counts`.
    pub fn set_test_counts(&mut self, passed: u64, failed: u64) {
        self.tests_passed = Some(passed);
        self.tests_failed = Some(failed);
    }

    /// Record that `name` was surfaced by pre-plan skill discovery (#173).
    ///
    /// Why: Discovery considers a broader set of skills than what is actually
    /// injected. Tracking the full candidate set separately preserves the
    /// recall side of the recall/precision picture for post-run analysis.
    /// What: Appends `name` to `skills_considered` if not already present
    /// (insertion-ordered de-dup).
    /// Test: `collector_records_skills_considered`.
    pub fn record_skill_considered(&mut self, name: &str) {
        if !self.skills_considered.iter().any(|s| s == name) {
            self.skills_considered.push(name.to_string());
        }
    }

    /// Record that `name` was injected into a phase prompt during this run
    /// (#171).
    ///
    /// Why: Post-run persistence (`update_skill_usage` in `main.rs`) needs the
    /// list of injected skills to update `use_count` and `last_used`. Tracking
    /// here keeps the collector the single source of truth for the run.
    /// What: Appends `name` to `skills_used` if not already present
    /// (insertion-ordered de-dup).
    /// Test: Indirect via `update_skill_usage` integration in main.
    pub fn record_skill_used(&mut self, name: &str) {
        if !self.skills_used.iter().any(|s| s == name) {
            self.skills_used.push(name.to_string());
        }
    }

    /// Set the run status (#56).
    ///
    /// Why: The workflow engine calls this after the phase loop to record
    /// "success" on clean completion and "partial" / "failed" when a phase
    /// returned an error, so the flushed JSON reflects reality.
    /// What: Stores the status string verbatim; `flush` serializes it.
    /// Test: `collector_flush_records_failed_status`.
    pub fn set_status(&mut self, status: &str) {
        self.status = status.to_string();
    }

    /// Record which phase failed (#56).
    ///
    /// Why: Paired with `set_status`, this lets analysis tooling group failed
    /// runs by failing phase without having to parse the last `phases` entry.
    /// What: Stores the phase name; included in the flushed JSON when set.
    /// Test: `collector_flush_records_failed_status`.
    pub fn set_failed_phase(&mut self, phase: &str) {
        self.failed_phase = Some(phase.to_string());
    }

    /// Record the dominant model used by the run for pricing.
    ///
    /// Why: The LLM pricing table is keyed by model substring. The workflow
    /// engine doesn't know the model up front (each phase's agent may use a
    /// different one), so we let the caller set/override as needed.
    /// What: Stores the latest model string; pricing is looked up per
    /// `record_phase` call using the `model` argument passed there, not this
    /// field (this is retained for future aggregate analysis).
    /// Test: implicit via `collector_flush_writes_json_and_log`.
    #[allow(dead_code)]
    pub fn set_model(&mut self, model: &str) {
        self.model = model.to_string();
    }

    /// Append a phase's measurements, computing its USD cost from `model`.
    ///
    /// Why: Engines call this once per phase with the agent model and the
    /// summed `TokenUsage` from all LLM turns in that phase.
    /// What: Computes `cost_usd` via `cost_usd(model, ...)`, pushes a
    /// `PhaseRecord`.
    /// Test: `collector_records_phases`.
    pub fn record_phase(&mut self, name: &str, duration_ms: u64, model: &str, usage: &TokenUsage) {
        let cost = cost_usd(
            model,
            usage.prompt_tokens,
            usage.completion_tokens,
            usage.cache_read_tokens,
            usage.cache_creation_tokens,
        );
        self.phases.push(PhaseRecord {
            name: name.to_string(),
            duration_ms,
            prompt_tokens: usage.prompt_tokens,
            completion_tokens: usage.completion_tokens,
            cache_read_tokens: usage.cache_read_tokens,
            cache_creation_tokens: usage.cache_creation_tokens,
            cost_usd: cost,
        });
    }

    /// Sum all phase records into a single `PerfTotals`.
    fn totals(&self) -> PerfTotals {
        let mut t = PerfTotals::default();
        for p in &self.phases {
            t.prompt_tokens = t.prompt_tokens.saturating_add(p.prompt_tokens);
            t.completion_tokens = t.completion_tokens.saturating_add(p.completion_tokens);
            t.cache_read_tokens = t.cache_read_tokens.saturating_add(p.cache_read_tokens);
            t.cache_creation_tokens = t
                .cache_creation_tokens
                .saturating_add(p.cache_creation_tokens);
            t.cost_usd += p.cost_usd;
        }
        t
    }

    /// Build the final `PerfRecord` without writing to disk.
    ///
    /// Why: Exposed for tests and for in-process consumers (the #151 JSON
    /// envelope projects this record into a `PmResponse`). `flush` calls
    /// this internally.
    pub fn build_record(&self) -> PerfRecord {
        let total_duration_ms = self.started_at.elapsed().as_millis() as u64;
        PerfRecord {
            build: self.build,
            version: env!("CARGO_PKG_VERSION").to_string(),
            workflow: self.workflow.clone(),
            task_preview: self.task_preview.clone(),
            started_at: self.started_at_iso.clone(),
            total_duration_ms,
            phases: self.phases.clone(),
            totals: self.totals(),
            status: self.status.clone(),
            failed_phase: self.failed_phase.clone(),
            skills_used: self.skills_used.clone(),
            skills_considered: self.skills_considered.clone(),
            tests_passed: self.tests_passed,
            tests_failed: self.tests_failed,
        }
    }

    /// Persist the record to `out_dir/runs/<stamp>.json` and append a
    /// summary line to `out_dir/runs.log`.
    ///
    /// Why: A single JSON per run is easy to diff and feed to tooling; the
    /// log line is a human-readable one-liner suitable for `tail -f`.
    /// What: Computes total duration, serializes to pretty JSON, writes the
    /// file atomically-ish (direct write is fine for append-only telemetry),
    /// then appends a one-line summary to `runs.log`.
    /// Test: `collector_flush_writes_json_and_log`.
    pub async fn flush(&self, out_dir: &Path) -> Result<()> {
        let record = self.build_record();
        let runs_dir = out_dir.join("runs");
        tokio::fs::create_dir_all(&runs_dir)
            .await
            .with_context(|| format!("failed to create {}", runs_dir.display()))?;

        // Stamp filename: YYYYMMDD-HHMMSS-build<N>.json
        // Derived from `started_at_iso` (UTC) so filenames are deterministic
        // given the same start time — useful for test fixtures.
        let stamp = filename_stamp(&self.started_at_iso, self.build);
        let json_path = runs_dir.join(format!("{stamp}.json"));
        let pretty = serde_json::to_vec_pretty(&record).context("serialize PerfRecord")?;
        tokio::fs::write(&json_path, &pretty)
            .await
            .with_context(|| format!("failed to write {}", json_path.display()))?;

        // One-line summary: tab-separated key=value pairs.
        // `tests_passed` / `tests_failed` are appended as the LAST two columns
        // for back-compat with log readers that only parse the first N
        // columns. Render `None` as "-" so the column is always present.
        let fmt_opt = |n: Option<u64>| -> String {
            n.map(|v| v.to_string()).unwrap_or_else(|| "-".to_string())
        };
        let line = format!(
            "{ts}\tbuild={b}\tworkflow={w}\tdur_ms={d}\tprompt={p}\tcompletion={c}\tcache_r={cr}\tcache_w={cc}\tcost_usd={cost:.6}\ttests_passed={tp}\ttests_failed={tf}\n",
            ts = self.started_at_iso,
            b = self.build,
            w = self.workflow,
            d = record.total_duration_ms,
            p = record.totals.prompt_tokens,
            c = record.totals.completion_tokens,
            cr = record.totals.cache_read_tokens,
            cc = record.totals.cache_creation_tokens,
            cost = record.totals.cost_usd,
            tp = fmt_opt(record.tests_passed),
            tf = fmt_opt(record.tests_failed),
        );
        let log_path = out_dir.join("runs.log");
        // Ensure parent exists (out_dir itself might not yet).
        if let Some(parent) = log_path.parent() {
            tokio::fs::create_dir_all(parent).await.ok();
        }
        // Append.
        use tokio::io::AsyncWriteExt;
        let mut f = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .await
            .with_context(|| format!("failed to open {}", log_path.display()))?;
        f.write_all(line.as_bytes())
            .await
            .context("failed to append perf log line")?;
        f.flush().await.ok();

        tracing::info!(
            path = %json_path.display(),
            build = self.build,
            total_ms = record.total_duration_ms,
            total_cost_usd = record.totals.cost_usd,
            "wrote perf record"
        );
        Ok(())
    }
}
