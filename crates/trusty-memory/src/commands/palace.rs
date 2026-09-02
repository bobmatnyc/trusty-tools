//! `trusty-memory palace stats` and `trusty-memory palace compact` (#6652).
//!
//! Why: #6652 opened with a 342 MB `kg.redb` and no way to ask what was in it.
//! `palace_info` reports drawer / room / wing counts; nothing reported the
//! file's size, its per-table row counts, or how much of it is the permanent
//! `hist:` rows every retraction leaves behind. `stats` is that missing report,
//! and it is the evidence the owner's "most of it is noise" ruling asked for
//! before any deletion logic ships. `compact` is the action those numbers
//! justify.
//!
//! What: `stats` opens the file through `ReadOnlyRedb` — `O_RDONLY` on the live
//! file, or a throw-away snapshot when the daemon holds it — so it can be run
//! against a production palace with the daemon up. `compact` needs the write
//! lock, so it refuses while the daemon holds the file rather than rewriting a
//! snapshot over the live store; `--dry-run` degrades to the same read-only
//! report `stats` prints.
//!
//! Test: `palace_stats_reports_a_hand_built_palace`,
//! `palace_compact_dry_run_writes_nothing`.

use anyhow::{Context, Result};
use clap::Subcommand;
use trusty_common::memory_core::dream::{kg_compact_pass, DreamConfig};
use trusty_common::memory_core::palace::Palace;
use trusty_common::memory_core::retrieval::PalaceHandle;
use trusty_common::memory_core::store::kg_redb::KgRedbStats;
use trusty_common::memory_core::store::OpenIntent;
use trusty_common::memory_core::PalaceRegistry;

/// Actions under `trusty-memory palace` (#6652).
///
/// Why: `kg.redb` growth needed both a read-only answer ("what is in there?")
/// and an action ("reclaim it"), and conflating them into one flag on the
/// existing vector-only `palace_compact` MCP tool would have widened that
/// tool's blast radius with nothing in its name to warn a caller.
/// What: `Stats` is always read-only. `Compact` writes unless `--dry-run`.
/// Test: `cargo run -p trusty-memory -- palace --help` lists both.
#[derive(Debug, Subcommand)]
pub enum PalaceAction {
    /// Report kg.redb's size, per-table row counts, and reclaimable estimate.
    ///
    /// READ-ONLY. Opens the file `O_RDONLY`, or — when the daemon holds the
    /// write lock — a throw-away snapshot, so it is safe to run against a live
    /// palace. It never opens a write transaction and never runs an at-open
    /// migration.
    Stats {
        /// Palace id (as listed by `trusty-memory monitor palaces`).
        name: String,
        /// Age in days at which a closed `hist:` row counts as stale.
        #[arg(long, value_name = "DAYS", default_value_t = 90)]
        history_days: i64,
        /// Emit JSON instead of the plain-text table.
        #[arg(long)]
        json: bool,
    },
    /// Prune stale history rows and rewrite kg.redb to reclaim disk.
    ///
    /// Needs the write lock: stop the daemon first, or the open degrades to a
    /// read-only snapshot and the rewrite refuses rather than replacing the
    /// live store with a rewritten copy of a copy. `--dry-run` never needs it.
    Compact {
        /// Palace id.
        name: String,
        /// Measure and report what would change; write nothing.
        #[arg(long)]
        dry_run: bool,
        /// Prune closed `hist:` rows older than this many days (floor: 7).
        #[arg(long, value_name = "DAYS", default_value_t = 90)]
        history_days: i64,
    },
}

/// Route one `palace` subcommand to its handler.
///
/// Why: keeping the match here rather than in `main.rs` keeps that file under
/// the 500-SLOC production cap, and puts the routing next to the handlers it
/// routes to.
/// What: one arm per [`PalaceAction`] variant.
/// Test: `cargo run -p trusty-memory -- palace --help`.
pub async fn dispatch(action: PalaceAction) -> Result<()> {
    match action {
        PalaceAction::Stats {
            name,
            history_days,
            json,
        } => handle_palace_stats(name, history_days, json).await,
        PalaceAction::Compact {
            name,
            dry_run,
            history_days,
        } => handle_palace_compact(name, dry_run, history_days).await,
    }
}

/// `trusty-memory palace stats <name>` — read-only measurement.
///
/// Why/What: see the module doc. Never writes; never opens a write
/// transaction; never runs an at-open migration.
/// Test: `palace_stats_reports_a_hand_built_palace`.
pub async fn handle_palace_stats(name: String, history_days: i64, json: bool) -> Result<()> {
    let palace = resolve(&name)?;
    let path = palace.data_dir.join("kg.redb");
    let stats = KgRedbStats::measure(&path, history_days)
        .with_context(|| format!("measure {}", path.display()))?;
    if json {
        println!("{}", render_json(&name, &stats)?);
    } else {
        print!("{}", render_text(&name, &stats));
    }
    Ok(())
}

/// `trusty-memory palace compact <name> [--dry-run]` — the kg.redb rewrite.
///
/// Why: an operator needs to reclaim the space now rather than wait for the
/// idle dreamer, and needs to see what a run would do before authorising it.
/// What: `--dry-run` runs the measurement and the gate and prints the verdict
/// without writing a byte — no backup, no temp file, no rename. Without it, the
/// full copy-then-swap runs; a daemon holding the file makes the handle
/// read-only and the rewrite refuses rather than replacing the live store with
/// a rewritten copy of a snapshot.
/// Test: `palace_compact_dry_run_writes_nothing`.
pub async fn handle_palace_compact(name: String, dry_run: bool, history_days: i64) -> Result<()> {
    let palace = resolve(&name)?;
    let intent = if dry_run {
        OpenIntent::ReadOnlyClient
    } else {
        OpenIntent::Writer
    };
    let handle = std::sync::Arc::new(
        PalaceHandle::open_with_intent(&palace, intent)
            .with_context(|| format!("open palace {}", palace.id))?,
    );
    let cfg = DreamConfig {
        prune_history_after_days: history_days,
        // An operator asking for a compaction by name has already made the
        // size judgement the idle gate exists to make for them.
        compact_min_bytes: 0,
        ..DreamConfig::default()
    };
    let report = kg_compact_pass(&handle, &cfg, dry_run).await?;
    println!("palace={name} {}", report.summary());
    if let Some(backup) = &report.backup {
        println!("  backup: {}", backup.display());
    }
    print!("{}", render_text(&name, &report.stats));
    if dry_run {
        println!("nothing was written — re-run without --dry-run to compact");
    }
    Ok(())
}

/// Look up one palace by id under the configured data root.
fn resolve(name: &str) -> Result<Palace> {
    let data_dir = trusty_common::resolve_data_dir("trusty-memory")
        .context("resolve trusty-memory data dir")?;
    let root = crate::resolve_palace_registry_dir(data_dir);
    PalaceRegistry::list_palaces(&root)
        .unwrap_or_default()
        .into_iter()
        .find(|p| p.id.0 == name)
        .with_context(|| format!("no palace named '{name}' under {}", root.display()))
}

/// The plain-text report.
///
/// Why: an operator reads this before deciding whether to compact, so it leads
/// with the two numbers that decide it — the file size and the reclaimable
/// estimate — and then shows the per-table breakdown behind them.
/// Test: `palace_stats_reports_a_hand_built_palace`.
fn render_text(name: &str, s: &KgRedbStats) -> String {
    let mut out = String::new();
    out.push_str(&format!("palace={name} kg.redb={}\n", s.path.display()));
    if s.from_snapshot {
        out.push_str(
            "  note: a writer holds the live file; these numbers come from a snapshot taken \
             just now\n",
        );
    }
    out.push_str(&format!(
        "  file_bytes            {}\n  reclaimable_estimate  {} ({}%)\n",
        s.file_bytes,
        s.reclaimable_bytes,
        percent(s.reclaimable_bytes, s.file_bytes)
    ));
    out.push_str(&format!(
        "  triples active={} history={} stale(>{}d)={} stale_bytes={}\n",
        s.triples_active,
        s.triples_history,
        s.history_cutoff_days,
        s.triples_history_stale,
        s.triples_history_stale_bytes
    ));
    out.push_str(&format!(
        "  closed-in-place={} superseded_drawers={}\n",
        s.triples_closed_in_place, s.superseded_drawers
    ));
    if let Some(dead) = &s.dead_predicate_index {
        out.push_str(&format!(
            "  dead index triples_by_predicate: {} row(s), {} live bytes — reclaimed by \
             the next compaction (#6652)\n",
            dead.rows,
            dead.live_bytes()
        ));
    }
    out.push_str(&format!(
        "  {:<24} {:>10} {:>12} {:>12} {:>12}\n",
        "table", "rows", "stored", "metadata", "fragmented"
    ));
    for t in &s.tables {
        out.push_str(&format!(
            "  {:<24} {:>10} {:>12} {:>12} {:>12}\n",
            t.name, t.rows, t.stored_bytes, t.metadata_bytes, t.fragmented_bytes
        ));
    }
    out
}

/// The same report as JSON, for scripts.
fn render_json(name: &str, s: &KgRedbStats) -> Result<String> {
    let tables: Vec<serde_json::Value> = s
        .tables
        .iter()
        .map(|t| {
            serde_json::json!({
                "name": t.name,
                "rows": t.rows,
                "stored_bytes": t.stored_bytes,
                "metadata_bytes": t.metadata_bytes,
                "fragmented_bytes": t.fragmented_bytes,
                "pages": t.pages,
            })
        })
        .collect();
    let v = serde_json::json!({
        "palace": name,
        "path": s.path,
        "from_snapshot": s.from_snapshot,
        "file_bytes": s.file_bytes,
        "reclaimable_bytes": s.reclaimable_bytes,
        "triples_active": s.triples_active,
        "triples_closed_in_place": s.triples_closed_in_place,
        "triples_history": s.triples_history,
        "triples_history_stale": s.triples_history_stale,
        "triples_history_stale_bytes": s.triples_history_stale_bytes,
        "history_cutoff_days": s.history_cutoff_days,
        "superseded_drawers": s.superseded_drawers,
        "tables": tables,
    });
    serde_json::to_string_pretty(&v).context("serialize palace stats")
}

/// `part` as a whole-number percentage of `whole`; `0` when `whole` is zero.
fn percent(part: u64, whole: u64) -> u64 {
    if whole == 0 {
        0
    } else {
        part.saturating_mul(100) / whole
    }
}
