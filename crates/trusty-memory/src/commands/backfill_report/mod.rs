//! `trusty-memory backfill-report` — ADR-0028 Migration step 3, the read-only
//! human-gated triage list.
//!
//! Why: ADR-0028 changes how *new* facts are written. It leaves the 2,016
//! drawers already on disk across 93 palaces exactly as they are — §"What this
//! does not fix" records that tier assignment for them is a classification
//! problem the existing tags cannot settle, so Migration step 3 makes backfill
//! read-only and human-gated instead of automatic. That decision has a
//! consequence the ADR states plainly and this command exists to serve: the
//! estate's current problem drawers are addressed by human action or not at all,
//! and this report is the only thing that tells a human which ones they are.
//!
//! What: ranks drawers by how many turns they actually reached, measured from
//! the enriched-prompt hook logs, and prints the evidence for each — id,
//! excerpt, age, injection frequency, importance and its decayed value, room and
//! palace. It emits no verdict (see `signals.rs` for why) and it writes nothing.
//!
//! Sub-modules:
//!   - `log_index`: recovers per-drawer injection counts from the hook logs.
//!   - `candidates`: reads drawers read-only, joins, ranks.
//!   - `signals`: the observations attached to each row.
//!   - `render`: stanza and JSON output.
//!
//! Test: see `tests.rs`.

pub mod candidates;
pub mod log_index;
pub mod render;
pub mod signals;

use anyhow::{Context, Result};

use log_index::InjectionIndex;

/// Default number of stanzas printed.
///
/// Why: the ADR's own triage framing is "the worst offenders first" — the top of
/// this list is where nearly all the recovered budget is. 25 is a reading
/// session, not a backlog.
const DEFAULT_LIMIT: usize = 25;

/// Options for one report run.
#[derive(Debug, Clone, Default)]
pub struct ReportOptions {
    /// Restrict to a single palace slug.
    pub palace: Option<String>,
    /// Stanzas (or JSON entries) to emit.
    pub limit: Option<usize>,
    /// Drop drawers injected fewer than this many times.
    pub min_injections: u64,
    /// Emit JSON instead of stanzas.
    pub json: bool,
    /// Override the hook-log directory (default `<data_root>/logs`).
    pub logs_dir: Option<std::path::PathBuf>,
}

/// CLI entry point.
///
/// Why: a thin shim so the testable surface is [`candidates::build_census`] and
/// [`log_index::InjectionIndex`] rather than a clap handler, matching the shape
/// `commands::rooms` already uses for the other read-only audit path.
/// What: resolves the data root, scans the hook logs, builds the ranked census,
/// and renders it to stdout. Nothing on this path opens a palace for writing.
/// Test: covered through the two functions above; the wiring itself is exercised
/// by running the command.
pub async fn handle_backfill_report(opts: ReportOptions) -> Result<()> {
    let data_dir = trusty_common::resolve_data_dir("trusty-memory")
        .context("resolve trusty-memory data dir")?;
    let logs_dir = opts
        .logs_dir
        .clone()
        .unwrap_or_else(|| data_dir.join("logs"));
    let registry_dir = crate::resolve_palace_registry_dir(data_dir);

    let index = InjectionIndex::scan_dir(&logs_dir)
        .with_context(|| format!("scan prompt logs in {}", logs_dir.display()))?;
    if index.saw_no_logs() {
        // #4891: a missing log directory makes every count 0, which reads
        // identically to "nothing is stale". Say which it is before printing.
        eprintln!(
            "warning: no enriched-prompts.*.jsonl found in {} — injection counts \
             will all be 0. This is missing data, not an absence of stale drawers.",
            logs_dir.display()
        );
    }
    let census = candidates::build_census(
        &registry_dir,
        &index,
        opts.palace.as_deref(),
        opts.min_injections,
    )?;

    let limit = opts.limit.unwrap_or(DEFAULT_LIMIT);
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    if opts.json {
        render::render_json(&mut out, &census, &index.stats, limit)
    } else {
        render::render_text(&mut out, &census, &index.stats, limit)
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
