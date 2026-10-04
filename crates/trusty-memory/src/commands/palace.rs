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

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Subcommand;
use trusty_common::memory_core::dream::{kg_compact_pass, DreamConfig};
use trusty_common::memory_core::palace::Palace;
use trusty_common::memory_core::retrieval::PalaceHandle;
use trusty_common::memory_core::store::kg_redb::KgRedbStats;
use trusty_common::memory_core::store::OpenIntent;
use trusty_common::memory_core::{MaintenanceLease, PalaceRegistry};

use super::maintenance_gate::{open_purging_under_lease, require_lease};

/// Actions under `trusty-memory palace` (#6652).
///
/// Why: `kg.redb` growth needed both a read-only answer ("what is in there?")
/// and an action ("reclaim it"), and conflating them into one flag on the
/// existing vector-only `palace_compact` MCP tool would have widened that
/// tool's blast radius with nothing in its name to warn a caller.
/// What: `Stats` is always read-only. `Compact` writes unless `--dry-run`.
/// `LegacyKg` writes only with `--apply` (#8434). `Deletions` is read-only (#8732).
/// `Reclaim` is a dry run with no apply step (#9140).
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
    /// Report, and with `--apply` import, drawers stranded in a pre-redb
    /// SQLite `kg.db` (#8434).
    ///
    /// Dry run by default: reads private copies of `kg.db` and kg.redb, so it
    /// is safe with the daemon up. `--apply` needs the write lock (stop the
    /// daemon first) and refuses a store it would have to recreate. A missing
    /// drawer whose content a live drawer already holds under another id is
    /// skipped and counted as `content_duplicates` unless
    /// `--include-content-duplicates` is passed. Nothing is ever deleted or
    /// renamed.
    LegacyKg {
        /// Palace id.
        name: String,
        /// Import the missing drawers (default: report only).
        #[arg(long)]
        apply: bool,
        /// With `--apply`, skip embedding the imported drawers.
        #[arg(long)]
        no_embed: bool,
        /// With `--apply`, also import missing drawers whose content a live
        /// drawer already holds under another id (default: skip them).
        #[arg(long, requires = "apply")]
        include_content_duplicates: bool,
        /// Skip the 8-token minimum alone, as `memory_note` does (#8434). The
        /// secret, blocklist, word-count and noise-pattern gates still apply.
        /// Works with and without `--apply`, so the dry run previews it.
        #[arg(long)]
        allow_short: bool,
    },
    /// List drawers the dream and purge passes deleted, with the reason and,
    /// for dedup, the surviving drawer and score (#8732).
    ///
    /// READ-ONLY. Reads the palace's `maintenance_deletions.jsonl`; safe with
    /// the daemon up. User deletions (`memory_forget`) are not listed.
    Deletions {
        /// Palace id.
        name: String,
        /// Only records naming this drawer, as removed or surviving side.
        #[arg(long, value_name = "UUID")]
        drawer: Option<uuid::Uuid>,
        /// Show at most this many of the newest matching records.
        #[arg(long, default_value_t = 50)]
        limit: usize,
        /// Emit JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// List what a reclaim would remove, and why, across every palace (#9140).
    ///
    /// DRY RUN ONLY. Lists `*.v2-incompatible` files, KG backups, empty
    /// palaces idle 30+ days, directories without `palace.json`, a stale
    /// `uds_addr`, and trusty-code fixture turn drawers, with path, size,
    /// palace and reason. Reads drawer tables from private copies; deletes
    /// nothing. This build has no apply step.
    Reclaim {
        /// Accepted for the documented spelling; the command is always a dry run.
        #[arg(long)]
        dry_run: bool,
        /// Emit the JSON manifest (sizes and mtimes per item) instead of text.
        #[arg(long)]
        json: bool,
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
        PalaceAction::LegacyKg {
            name,
            apply,
            no_embed,
            include_content_duplicates,
            allow_short,
        } => {
            let (data_root, palace) = resolve(&name)?;
            let report = if apply {
                super::legacy_kg::apply_report(
                    &palace,
                    &data_root,
                    !no_embed,
                    include_content_duplicates,
                    allow_short,
                )
                .await?
            } else {
                let mut report = super::legacy_kg::scan_report(&palace, allow_short)?;
                // #8729 review: the dry run describes the apply these flags run.
                report.no_embed = no_embed;
                report.include_content_duplicates = include_content_duplicates;
                report
            };
            print!("{}", report.render());
            // #8434: the import committed; print it before failing on embed.
            match report.embed_error {
                Some(e) => anyhow::bail!("embed imported drawers: {e}"),
                None => Ok(()),
            }
        }
        PalaceAction::Deletions {
            name,
            drawer,
            limit,
            json,
        } => {
            let (_, palace) = resolve(&name)?;
            let report =
                super::palace_deletions::deletions_report(&name, &palace, drawer, limit, json)?;
            print!("{report}");
            Ok(())
        }
        // #9140: read-only; ruling b9 keeps any delete path out of this build.
        PalaceAction::Reclaim { dry_run: _, json } => {
            tokio::task::spawn_blocking(move || super::palace_reclaim::handle_reclaim(json))
                .await
                .context("join palace reclaim")?
        }
    }
}

/// `trusty-memory palace stats <name>` — read-only measurement.
///
/// Why/What: see the module doc. Never writes; never opens a write
/// transaction; never runs an at-open migration.
/// Test: `palace_stats_reports_a_hand_built_palace`.
pub async fn handle_palace_stats(name: String, history_days: i64, json: bool) -> Result<()> {
    let (_, palace) = resolve(&name)?;
    print!("{}", stats_report(&name, &palace, history_days, json)?);
    Ok(())
}

/// The report [`handle_palace_stats`] prints, as a string.
///
/// Why: `resolve` reads the machine's real data root, so a test driving the
/// handler would assert against whatever palaces that machine happens to hold.
/// Taking the `Palace` as an argument is what makes the measurement and the
/// rendering testable against a fixture.
/// What: measures `<data_dir>/kg.redb` read-only and renders text or JSON.
/// Test: `palace_stats_reports_a_hand_built_palace`.
pub(crate) fn stats_report(
    name: &str,
    palace: &Palace,
    history_days: i64,
    json: bool,
) -> Result<String> {
    let path = palace.data_dir.join("kg.redb");
    let stats = KgRedbStats::measure(&path, history_days)
        .with_context(|| format!("measure {}", path.display()))?;
    if json {
        Ok(format!("{}\n", render_json(name, &stats)?))
    } else {
        Ok(render_text(name, &stats))
    }
}

/// `trusty-memory palace compact <name> [--dry-run]` — the kg.redb rewrite.
///
/// Why: an operator needs to reclaim the space now rather than wait for the
/// idle dreamer, and needs to see what a run would do before authorising it.
/// What: `--dry-run` runs the measurement and the gate and prints the verdict
/// without writing a byte — no backup, no temp file, no rename. Without it, the
/// full copy-then-swap runs; a daemon holding the file makes the handle
/// read-only and the rewrite refuses rather than replacing the live store with
/// a rewritten copy of a snapshot. A real run also refuses while another
/// process holds the data root's maintenance lease (#8733).
/// Test: `palace_compact_dry_run_writes_nothing`,
/// `compact_under_a_lease_held_elsewhere_refuses_and_deletes_nothing`.
pub async fn handle_palace_compact(name: String, dry_run: bool, history_days: i64) -> Result<()> {
    let (data_root, palace) = resolve(&name)?;
    print!(
        "{}",
        compact_report(&name, &palace, &data_root, dry_run, history_days).await?
    );
    Ok(())
}

/// The report [`handle_palace_compact`] prints, as a string.
///
/// Why: same reason [`stats_report`] exists — the handler's only untestable
/// step is `resolve`, so the work moves below it.
/// What: opens the palace (Writer intent for a real run, read-only for a dry
/// run), runs the phase with the idle size gate disabled, and renders.
/// `data_root` is the registry dir whose `maintenance.lock` elects the one
/// maintainer.
/// Test: `palace_compact_dry_run_writes_nothing`,
/// `compact_under_a_lease_held_elsewhere_refuses_and_deletes_nothing`.
pub(crate) async fn compact_report(
    name: &str,
    palace: &Palace,
    data_root: &Path,
    dry_run: bool,
    history_days: i64,
) -> Result<String> {
    let lease = MaintenanceLease::new(data_root);
    let handle = if dry_run {
        // #8733: a dry run deletes nothing, including the open-time purge.
        PalaceHandle::open_with_intent_purging(palace, OpenIntent::ReadOnlyClient, false)
            .with_context(|| format!("open palace {}", palace.id))?
    } else {
        // #8733: compaction is maintenance through and through, so without
        // the lease it refuses rather than skipping; the lease stays held
        // until the pass returns.
        require_lease(&lease)?;
        open_purging_under_lease(palace, OpenIntent::Writer, &lease)?
    };
    let cfg = DreamConfig {
        prune_history_after_days: history_days,
        // An operator asking for a compaction by name has already made the
        // size judgement the idle gate exists to make for them.
        compact_min_bytes: 0,
        ..DreamConfig::default()
    };
    let report = kg_compact_pass(&handle, &cfg, dry_run).await?;
    let mut out = format!("palace={name} {}\n", report.summary());
    if let Some(backup) = &report.backup {
        out.push_str(&format!("  backup: {}\n", backup.display()));
    }
    out.push_str(&render_text(name, &report.stats));
    if dry_run {
        out.push_str("nothing was written — re-run without --dry-run to compact\n");
    }
    Ok(out)
}

/// Look up one palace by id under the configured data root, returning that
/// root (the registry dir, where `maintenance.lock` lives) with it.
fn resolve(name: &str) -> Result<(PathBuf, Palace)> {
    let data_dir = trusty_common::resolve_data_dir("trusty-memory")
        .context("resolve trusty-memory data dir")?;
    let root = crate::resolve_palace_registry_dir(data_dir);
    let palace = PalaceRegistry::list_palaces(&root)
        .unwrap_or_default()
        .into_iter()
        .find(|p| p.id.0 == name)
        .with_context(|| format!("no palace named '{name}' under {}", root.display()))?;
    Ok((root, palace))
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
///
/// `checked_div` rather than a zero guard: clippy's `manual_checked_division`
/// fires on the guarded form under the workspace-wide lint job.
fn percent(part: u64, whole: u64) -> u64 {
    part.saturating_mul(100).checked_div(whole).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use trusty_common::memory_core::palace::PalaceId;

    /// A palace directory on disk with `n` live triples in its `kg.redb`.
    fn fixture(name: &str, n: usize) -> (tempfile::TempDir, Palace) {
        let dir = tempfile::tempdir().expect("tempdir");
        let data_dir = dir.path().join(name);
        std::fs::create_dir_all(&data_dir).expect("mkdir");
        let kg = trusty_common::memory_core::store::kg_redb::KgStoreRedb::open(
            &data_dir.join("kg.redb"),
        )
        .expect("open kg");
        for i in 0..n {
            kg.assert(&trusty_common::memory_core::store::Triple {
                subject: format!("s{i}"),
                predicate: "knows".into(),
                object: format!("o{i}"),
                valid_from: chrono::Utc::now(),
                valid_to: None,
                confidence: 1.0,
                provenance: None,
            })
            .expect("assert");
        }
        drop(kg);
        let palace = Palace {
            id: PalaceId::new(name),
            name: name.into(),
            description: None,
            created_at: chrono::Utc::now(),
            data_dir,
        };
        (dir, palace)
    }

    /// Why: the report is the evidence #6652's "measure before deleting" gate
    /// rests on, so the numbers it prints have to be the palace's real ones —
    /// not a plausible-looking template.
    #[test]
    fn palace_stats_reports_a_hand_built_palace() {
        let (_d, palace) = fixture("stats-fixture", 5);
        let text = stats_report("stats-fixture", &palace, 90, false).expect("report");
        assert!(text.contains("palace=stats-fixture"), "{text}");
        assert!(text.contains("triples active=5"), "{text}");
        assert!(text.contains("history=0"), "{text}");
        assert!(text.contains("file_bytes"), "{text}");
        assert!(text.contains("triples_by_object"), "{text}");

        let json = stats_report("stats-fixture", &palace, 90, true).expect("json");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("parse");
        assert_eq!(parsed["triples_active"], 5);
        assert_eq!(parsed["palace"], "stats-fixture");
    }

    /// Why: `--dry-run` is the operator's look-before-you-leap, and it is only
    /// worth anything if it provably writes nothing — no backup, no temp file,
    /// no rename.
    #[tokio::test]
    async fn palace_compact_dry_run_writes_nothing() {
        let (_d, palace) = fixture("dry-run-fixture", 4);
        let kg_path = palace.data_dir.join("kg.redb");
        // Row counts, not raw bytes: redb rewrites its own allocator state when
        // a `Database` is dropped, so opening the palace at all changes the
        // file even when nothing wrote a row.
        let rows = |p: &std::path::Path| {
            KgRedbStats::measure(p, 90)
                .expect("measure")
                .tables
                .iter()
                .map(|t| (t.name.clone(), t.rows))
                .collect::<Vec<_>>()
        };
        let before = rows(&kg_path);

        let root = palace.data_dir.parent().expect("root").to_path_buf();
        let out = compact_report("dry-run-fixture", &palace, &root, true, 90)
            .await
            .expect("dry run");
        assert!(out.contains("dry-run:"), "{out}");
        assert!(out.contains("nothing was written"), "{out}");

        assert_eq!(before, rows(&kg_path), "the dry run changed kg.redb");
        assert!(!palace.data_dir.join("kg.redb.pre-compact.bak").exists());
        assert!(!palace.data_dir.join("kg.redb.compacting").exists());
    }

    /// #8733: compaction is maintenance, so while another process holds the
    /// data root's lease a real run refuses and neither run deletes the
    /// expired row. Pre-fix the real run compacted and its Writer open purged.
    #[tokio::test]
    async fn compact_under_a_lease_held_elsewhere_refuses_and_deletes_nothing() {
        use trusty_common::memory_core::palace::Drawer;
        use trusty_common::memory_core::store::kg_redb::KgStoreRedb;
        let (dir, palace) = fixture("lease-fixture", 2);
        let kg_path = palace.data_dir.join("kg.redb");
        let mut expired = Drawer::new(uuid::Uuid::new_v4(), "an expired drawer");
        expired.expires_at = Some(chrono::Utc::now() - chrono::Duration::days(1));
        KgStoreRedb::open(&kg_path)
            .expect("open kg")
            .upsert_drawer(&expired)
            .expect("seed expired drawer");
        let holder = MaintenanceLease::new(dir.path());
        assert!(holder.try_hold().is_held());

        compact_report("lease-fixture", &palace, dir.path(), true, 90)
            .await
            .expect("a dry run needs no lease");
        let err = compact_report("lease-fixture", &palace, dir.path(), false, 90)
            .await
            .expect_err("a real run must refuse without the lease");
        assert!(format!("{err:#}").contains("maintenance lease"), "{err:#}");

        let ids = KgStoreRedb::open(&kg_path)
            .expect("reopen kg")
            .load_drawer_ids()
            .expect("ids");
        assert!(ids.contains(&expired.id), "the expired row was deleted");
        assert!(!palace.data_dir.join("kg.redb.pre-compact.bak").exists());
    }
}
