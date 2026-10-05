//! The live leg of the model-eval harness: real Bedrock reviewers, a Haiku 4.5
//! verifier, a hard cost cap, and a JSON report (Q86 ruling).
//!
//! Why: choosing the reviewer default needs recall, false positives and cost
//! per model, measured on the same 40 labelled diffs. That costs real money,
//! so it runs only on an explicit opt-in and stops at a cap.
//! What: [`live_settings`] reads the environment and returns `None` unless
//! `TRUSTY_EVAL_LIVE=1`; [`run_live`] checks every model is priced, then runs
//! passes x models x diffs through [`review_diff`] with providers from the
//! injected factory, stops when the shared [`Budget`] is spent, writes
//! `<out>/<UTC ts>.json` and prints a markdown summary. The factory is the
//! only place a network provider is built, and it is never called when the
//! opt-in is absent.
//! Test: `live_leg_is_inert_without_opt_in`, `live_settings_read_overrides`,
//! `live_leg_refuses_an_unpriced_model`, `live_leg_writes_a_report_and_stops_at_the_cap`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures_util::stream::{self, StreamExt};
use serde::Serialize;
use trusty_review::llm::bedrock::estimate_bedrock_cost_usd;
use trusty_review::llm::models::{COMPARE_CANDIDATE_MODELS, DEFAULT_VERIFIER_MODEL};
use trusty_review::llm::{LlmProvider, strip_provider_prefix};

use crate::eval::{Entry, Row, Totals, eval_config, load_dataset, markdown_table, review_diff};
use crate::metered::Budget;

/// Default cap on reviewer plus verifier spend, USD (Q86 ruling).
pub const DEFAULT_MAX_USD: f64 = 22.00;
/// Default passes per model (Q86 ruling).
pub const DEFAULT_PASSES: u32 = 3;
/// Default reviews in flight per model and pass.
pub const DEFAULT_CONCURRENCY: usize = 4;

/// Reads one environment variable; injected so tests never touch the process
/// environment.
pub type Env<'a> = &'a dyn Fn(&str) -> Option<String>;

/// Builds a provider for a model id; injected so tests never build a network
/// provider.
pub type Factory<'a> = &'a (dyn Fn(&str) -> Result<Arc<dyn LlmProvider>, String> + Sync);

/// What one live run is configured to do.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LiveSettings {
    /// Reviewer model ids (`TRUSTY_EVAL_MODELS`, comma-separated).
    pub models: Vec<String>,
    /// Verifier model id (Haiku 4.5).
    pub verifier: String,
    /// Passes per model (`TRUSTY_EVAL_PASSES`).
    pub passes: u32,
    /// Spend cap, USD (`TRUSTY_EVAL_MAX_USD`).
    pub max_usd: f64,
    /// Reviews in flight (`TRUSTY_EVAL_CONCURRENCY`).
    pub concurrency: usize,
    /// Only these entry ids, when set (`TRUSTY_EVAL_ONLY`, comma-separated).
    pub only: Vec<String>,
    /// Report directory (`TRUSTY_EVAL_OUT_DIR`, else `<target>/model_eval`).
    pub out_dir: PathBuf,
}

fn list(raw: Option<String>) -> Vec<String> {
    raw.unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

fn parsed<T: std::str::FromStr>(env: Env<'_>, key: &str, default: T) -> Result<T, String> {
    match env(key).filter(|v| !v.trim().is_empty()) {
        None => Ok(default),
        Some(v) => v
            .trim()
            .parse()
            .map_err(|_| format!("{key}={v} does not parse")),
    }
}

/// The workspace target directory: `CARGO_TARGET_DIR`, else `<repo>/target`.
fn target_dir(env: Env<'_>) -> PathBuf {
    env("CARGO_TARGET_DIR").map_or_else(
        || Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target"),
        PathBuf::from,
    )
}

/// The live settings, or `None` unless `TRUSTY_EVAL_LIVE=1`.
pub fn live_settings(env: Env<'_>) -> Result<Option<LiveSettings>, String> {
    if env("TRUSTY_EVAL_LIVE").as_deref() != Some("1") {
        return Ok(None);
    }
    let mut models = list(env("TRUSTY_EVAL_MODELS"));
    if models.is_empty() {
        models = COMPARE_CANDIDATE_MODELS
            .iter()
            .map(|m| m.to_string())
            .collect();
    }
    let passes = parsed(env, "TRUSTY_EVAL_PASSES", DEFAULT_PASSES)?;
    let max_usd = parsed(env, "TRUSTY_EVAL_MAX_USD", DEFAULT_MAX_USD)?;
    let concurrency = parsed(env, "TRUSTY_EVAL_CONCURRENCY", DEFAULT_CONCURRENCY)?;
    if passes == 0 || concurrency == 0 || max_usd.is_nan() || max_usd <= 0.0 {
        return Err("TRUSTY_EVAL_PASSES, _CONCURRENCY and _MAX_USD must be positive".into());
    }
    let out_dir = env("TRUSTY_EVAL_OUT_DIR")
        .map_or_else(|| target_dir(env).join("model_eval"), PathBuf::from);
    Ok(Some(LiveSettings {
        models,
        verifier: DEFAULT_VERIFIER_MODEL.to_string(),
        passes,
        max_usd,
        concurrency,
        only: list(env("TRUSTY_EVAL_ONLY")),
        out_dir,
    }))
}

/// Whether `model` has a Bedrock price; an unpriced model would meter as $0
/// and slip past the cap.
pub fn is_priced(model: &str) -> bool {
    estimate_bedrock_cost_usd(strip_provider_prefix(model), 1_000_000, 1_000_000) > 0.0
}

/// How a live run ended.
#[derive(Debug)]
pub enum LiveOutcome {
    /// `TRUSTY_EVAL_LIVE` was not `1`; nothing was built or called.
    Skipped,
    /// The run finished or stopped at the cap.
    Ran {
        /// The JSON report.
        report: PathBuf,
        /// Total spend, USD.
        spent_usd: f64,
        /// Why the run stopped early, if it did.
        stopped: Option<String>,
        /// Rows written.
        rows: usize,
    },
}

fn git_sha() -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// One model's reviews over `entries` for one pass, `concurrency` at a time.
async fn model_pass(
    settings: &LiveSettings,
    entries: &[Entry],
    model: &str,
    pass: u32,
    factory: Factory<'_>,
    budget: &Arc<Budget>,
) -> Result<Vec<Row>, String> {
    let config = eval_config(&settings.verifier);
    let mut jobs = Vec::with_capacity(entries.len());
    for entry in entries {
        jobs.push((entry, factory(model)?, factory(&settings.verifier)?));
    }
    let mut rows: Vec<Row> = stream::iter(jobs)
        .map(|(entry, llm, verifier)| {
            review_diff(&config, entry, model, pass, llm, verifier, budget)
        })
        .buffer_unordered(settings.concurrency)
        .collect()
        .await;
    rows.sort_by(|a, b| a.diff.cmp(&b.diff));
    Ok(rows)
}

/// Run the live comparison, or return [`LiveOutcome::Skipped`] without
/// building a provider when the opt-in is absent.
pub async fn run_live(env: Env<'_>, factory: Factory<'_>) -> Result<LiveOutcome, String> {
    let Some(settings) = live_settings(env)? else {
        return Ok(LiveOutcome::Skipped);
    };
    let unpriced: Vec<&String> = settings
        .models
        .iter()
        .chain([&settings.verifier])
        .filter(|m| !is_priced(m))
        .collect();
    if !unpriced.is_empty() {
        return Err(format!(
            "no Bedrock price for {unpriced:?}; the cost cap cannot meter them"
        ));
    }
    let entries: Vec<Entry> = load_dataset()
        .into_iter()
        .filter(|e| settings.only.is_empty() || settings.only.contains(&e.id))
        .collect();
    let started = chrono::Utc::now();
    let budget = Budget::new(settings.max_usd);
    let (mut rows, mut stopped) = (Vec::new(), None);
    'passes: for pass in 1..=settings.passes {
        for model in &settings.models {
            rows.extend(model_pass(&settings, &entries, model, pass, factory, &budget).await?);
            if budget.exhausted() {
                stopped = Some(format!(
                    "cost cap ${:.2} reached after pass {pass} of {model}",
                    settings.max_usd
                ));
                break 'passes;
            }
        }
    }
    let summary: Vec<(String, u32, Totals)> = settings
        .models
        .iter()
        .map(|m| {
            let mine: Vec<&Row> = rows.iter().filter(|r| &r.model == m).collect();
            let passes = mine.iter().map(|r| r.pass).max().unwrap_or(0);
            (m.clone(), passes, Totals::of(mine))
        })
        .collect();
    let table = markdown_table(&summary);
    println!("{table}");
    if let Some(why) = &stopped {
        println!("STOPPED: {why}");
    }
    let report = serde_json::json!({
        "config": settings,
        "git_sha": git_sha(),
        "dataset": { "entries": entries.len() },
        "started_utc": started.to_rfc3339(),
        "finished_utc": chrono::Utc::now().to_rfc3339(),
        "spent_usd": budget.spent(),
        "stopped_reason": stopped,
        "summary": summary.iter().map(|(model, passes, totals)| serde_json::json!({
            "model": model, "passes": passes, "recall": totals.recall(), "totals": totals,
        })).collect::<Vec<_>>(),
        "rows": rows,
    });
    std::fs::create_dir_all(&settings.out_dir)
        .map_err(|e| format!("cannot create {}: {e}", settings.out_dir.display()))?;
    let path = settings
        .out_dir
        .join(format!("{}.json", started.format("%Y%m%dT%H%M%SZ")));
    let text = serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?;
    std::fs::write(&path, text).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    println!("report: {}", path.display());
    Ok(LiveOutcome::Ran {
        report: path,
        spent_usd: budget.spent(),
        stopped,
        rows: rows.len(),
    })
}
