//! `trusty-memory import kuzu` — discover kuzu-memory stores and import them,
//! knowledge-graph edges included, idempotently (#277).
//!
//! Why: kuzu-memory is retired, and its users hold one store per project. The
//! owner asked for an importer that finds them and can be re-run safely.
//!
//! What, end to end, per store:
//! 1. [`discovery`] finds `<project>/.kuzu-memory/memories.db` (or `--from`).
//! 2. [`bridge`] runs `export.py` under kuzu-memory's own interpreter with a
//!    read-only database open; the JSON lands in a temp dir removed on return.
//! 3. [`mapping`] turns rows into drawers and triples; [`ledger`] reads what
//!    the palace already holds from the drawers' own tags.
//! 4. [`apply`] plans, then writes (or, with `--dry-run`, only counts).
//!
//! **Palace mapping (deterministic).** A discovered store imports into
//! `trusty_common::palace_resolve::resolve_palace(<project dir>)`, where the
//! project dir is the parent of `.kuzu-memory` — the same rule every other
//! trusty-memory entry point uses: `TRUSTY_MEMORY_PALACE`, then a committed
//! palace pin, then the git `owner/repo` slug, then the `parent/dir` slug. The
//! same store resolves to the same palace on every run. `--palace` overrides
//! it, and is accepted only with `--from`, where there is exactly one store.
//!
//! **Identity and idempotency.** Every imported drawer carries
//! `source:kuzu-memory/<store-id>/<Memory.id>` (store id: 12 hex chars of
//! SHA-256 over the canonical `.kuzu-memory` path) and `kuzu-hash:<hash>`.
//! A re-run skips a memory whose hash matches, flags one whose hash differs as
//! changed, and rewrites that same drawer only under `--update`. A triple is
//! asserted only when the exact `(subject, predicate, object)` is not already
//! active. There is no separate "done" record, so a run that fails part-way
//! can never be mistaken for a finished one: the next run imports the rest.
//!
//! **KG shape.** Entity `e` -> `entity:<e.id>` with `has_name` and
//! `entity_type` triples; MENTIONS -> `drawer:<uuid> mentions entity:<id>`;
//! RELATES_TO -> `drawer:<a> relates_to:<relationship_type> drawer:<b>`.
//!
//! **Writes need the daemon stopped.** The import opens the palace in this
//! process; while the daemon holds the palace's write lock the open is a
//! read-only snapshot and the store fails with [`KuzuImportError::PalaceLocked`]
//! before anything is written. A dry run reads without that lock.
//!
//! Test: `kuzu_import::tests`.

pub mod apply;
pub mod bridge;
pub mod discovery;
pub mod ledger;
pub mod mapping;
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use colored::Colorize;
use trusty_common::memory_core::palace::{Palace, PalaceId};
use trusty_common::memory_core::store::{KnowledgeGraph, OpenIntent};
use trusty_common::memory_core::{PalaceHandle, PalaceRegistry};

use apply::{execute, plan_store, HandleSink, PalaceSink, PalaceView, SnapshotView, StoreCounts};
use bridge::{export_store, resolve_python, CommandRunner, KuzuExport, SystemRunner};
use discovery::{discover, resolve_from, DiscoveredStore};
use ledger::Ledger;

/// Default walk depth under each root, matching `find -maxdepth 5`.
pub const DEFAULT_MAX_DEPTH: usize = 5;

/// Everything that can stop one store's import.
///
/// Why: each arm is a distinct operator action (install kuzu-memory, stop the
/// daemon, upgrade the store), so each gets its own variant and message.
#[derive(Debug, thiserror::Error)]
pub enum KuzuImportError {
    #[error("kuzu-memory interpreter not found: {0}")]
    InterpreterNotFound(String),
    #[error("export bridge exited with status {code:?}: {stderr}")]
    BridgeFailed { code: Option<i32>, stderr: String },
    #[error("export output is malformed: {0}")]
    MalformedExport(String),
    #[error("store lacks required Memory columns: {}", .0.join(", "))]
    SchemaColumnsMissing(Vec<String>),
    #[error("{} is not a kuzu-memory store (expected a .kuzu-memory dir or memories.db)", .0.display())]
    NotAStore(PathBuf),
    #[error("could not resolve a palace: {0}")]
    PalaceResolve(String),
    #[error(
        "palace '{0}' is locked by the running daemon; stop it (`trusty-memory stop`) and re-run"
    )]
    PalaceLocked(String),
    #[error("palace write failed: {0}")]
    Palace(String),
    #[error("I/O: {0}")]
    Io(String),
}

impl KuzuImportError {
    /// The variant name, for logs that must not carry the message text.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::InterpreterNotFound(_) => "interpreter_not_found",
            Self::BridgeFailed { .. } => "bridge_failed",
            Self::MalformedExport(_) => "malformed_export",
            Self::SchemaColumnsMissing(_) => "schema_columns_missing",
            Self::NotAStore(_) => "not_a_store",
            Self::PalaceResolve(_) => "palace_resolve",
            Self::PalaceLocked(_) => "palace_locked",
            Self::Palace(_) => "palace",
            Self::Io(_) => "io",
        }
    }
}

/// `trusty-memory import <source>`.
#[derive(Debug, Clone, clap::Subcommand)]
pub enum ImportSource {
    /// Import kuzu-memory stores, knowledge-graph edges included.
    Kuzu(KuzuImportArgs),
}

/// Flags for `trusty-memory import kuzu`.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct KuzuImportArgs {
    /// Walk $HOME (plus every --root) for `.kuzu-memory` stores.
    #[arg(long)]
    pub discover: bool,
    /// Also walk this directory (repeatable). Without --discover, walk only these.
    #[arg(long, value_name = "PATH")]
    pub root: Vec<PathBuf>,
    /// Import one store: a `.kuzu-memory` directory or its `memories.db`.
    #[arg(long, value_name = "PATH", conflicts_with_all = ["discover", "root"])]
    pub from: Option<PathBuf>,
    /// Target palace for --from (default: resolved from the project directory).
    #[arg(long, value_name = "NAME", requires = "from")]
    pub palace: Option<String>,
    /// Report what would be imported; write nothing.
    #[arg(long)]
    pub dry_run: bool,
    /// Rewrite drawers whose memory changed in kuzu since the last import.
    #[arg(long)]
    pub update: bool,
    /// Walk depth under each root.
    #[arg(long, value_name = "N", default_value_t = DEFAULT_MAX_DEPTH)]
    pub max_depth: usize,
    /// Python interpreter that can `import kuzu` (default: kuzu-memory's own).
    #[arg(long, value_name = "PATH")]
    pub python: Option<PathBuf>,
}

/// How one store ended.
#[derive(Debug)]
pub enum StoreStatus {
    Imported,
    WouldImport,
    UpToDate,
    Empty,
    /// Some writes failed; a re-run picks up the rest.
    Partial,
    Failed(KuzuImportError),
}

/// One store's outcome.
#[derive(Debug)]
pub struct StoreReport {
    pub store: PathBuf,
    pub palace: Option<String>,
    pub counts: StoreCounts,
    pub status: StoreStatus,
}

/// Where a store's plan is applied.
pub enum Target<'a> {
    /// Count only; nothing is written.
    DryRun(&'a dyn PalaceView),
    Write(&'a dyn PalaceSink),
}

/// Entry point for `trusty-memory import <source>`.
pub async fn handle_import(source: ImportSource) -> anyhow::Result<()> {
    match source {
        ImportSource::Kuzu(args) => handle_import_kuzu(args).await,
    }
}

/// Entry point for `trusty-memory import kuzu`.
///
/// Why: the one CLI path, shared with the deprecated `migrate kuzu-data`.
/// What: select stores, resolve the interpreter once, import each store in
/// turn, print one line per store and a total. Exits non-zero when any store
/// failed or was only partly written; a changed-but-not-updated memory is a
/// notice, not a failure.
/// Test: `kuzu_import::tests` drives each stage; the CLI shape is covered by
/// `import_kuzu_cli_parses_flags`.
pub async fn handle_import_kuzu(args: KuzuImportArgs) -> anyhow::Result<()> {
    let stores = select_stores(&args)?;
    if stores.is_empty() {
        println!("No kuzu-memory stores found.");
        return Ok(());
    }
    let python = resolve_python(args.python.as_deref(), std::env::var_os("PATH").as_deref())?;
    let data_dir = trusty_common::resolve_data_dir("trusty-memory")?;
    let data_root = crate::resolve_palace_registry_dir(data_dir);
    if args.dry_run {
        println!("{} Dry run — nothing will be written.", "·".dimmed());
    }
    let mut reports = Vec::new();
    for store in &stores {
        let report = import_one(&SystemRunner, &python, store, &args, &data_root).await;
        print_report(&report);
        reports.push(report);
    }
    print_totals(&reports, args.update);
    let bad = reports
        .iter()
        .filter(|r| matches!(r.status, StoreStatus::Failed(_) | StoreStatus::Partial))
        .count();
    if bad > 0 {
        anyhow::bail!(
            "{bad} of {} store(s) did not import completely",
            reports.len()
        );
    }
    Ok(())
}

/// The stores `args` names: `--from`, or a walk of the roots.
fn select_stores(args: &KuzuImportArgs) -> anyhow::Result<Vec<DiscoveredStore>> {
    if let Some(from) = &args.from {
        return Ok(vec![resolve_from(from)?]);
    }
    if !args.discover && args.root.is_empty() {
        anyhow::bail!("name the stores: --discover, --root <path>, or --from <path>");
    }
    let mut roots = args.root.clone();
    if args.discover {
        roots.insert(
            0,
            dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no home directory"))?,
        );
    }
    let found = discover(&roots, args.max_depth);
    for s in &found.skipped {
        eprintln!(
            "{} skipped {}: {}",
            "·".dimmed(),
            s.path.display(),
            s.reason
        );
    }
    Ok(found.stores)
}

/// Import one store end to end, opening the palace only when there is data.
async fn import_one(
    runner: &dyn CommandRunner,
    python: &Path,
    store: &DiscoveredStore,
    args: &KuzuImportArgs,
    data_root: &Path,
) -> StoreReport {
    let mut report = StoreReport {
        store: store.dir.clone(),
        palace: None,
        counts: StoreCounts::default(),
        status: StoreStatus::Empty,
    };
    let fetched = fetch(runner, python, store).and_then(|export| {
        let palace = target_palace(store, args.palace.as_deref())?;
        Ok((export, palace))
    });
    let (export, palace) = match fetched {
        Ok(v) => v,
        Err(e) => {
            report.status = StoreStatus::Failed(e);
            return report;
        }
    };
    report.palace = Some(palace.clone());
    if is_empty(&export) {
        return report;
    }
    let store_id = mapping::store_id(&store.dir);
    let counts = if args.dry_run {
        match open_snapshot_view(data_root, &palace) {
            Ok(view) => run_plan(&export, &store_id, Target::DryRun(&view), args.update).await,
            Err(e) => {
                report.status = StoreStatus::Failed(e);
                return report;
            }
        }
    } else {
        match open_palace_for_write(data_root, &palace) {
            Ok(handle) => {
                let sink = HandleSink { handle };
                let counts = run_plan(&export, &store_id, Target::Write(&sink), args.update).await;
                if let Err(e) = sink.handle.flush() {
                    tracing::warn!("kuzu import: palace flush failed: {e:#}");
                }
                counts
            }
            Err(e) => {
                report.status = StoreStatus::Failed(e);
                return report;
            }
        }
    };
    report.counts = counts;
    report.status = status_for(&report.counts, args.dry_run);
    report
}

/// Run the bridge for one store.
pub fn fetch(
    runner: &dyn CommandRunner,
    python: &Path,
    store: &DiscoveredStore,
) -> Result<KuzuExport, KuzuImportError> {
    export_store(runner, python, &store.db)
}

/// Whether an export carries nothing to import.
pub fn is_empty(export: &KuzuExport) -> bool {
    export.memories.is_empty() && export.entities.is_empty() && export.edge_count() == 0
}

/// Plan `export` against `target`'s current state and apply it.
///
/// Test: `import_twice_is_idempotent_on_palace_state`.
pub async fn run_plan(
    export: &KuzuExport,
    store_id: &str,
    target: Target<'_>,
    update: bool,
) -> StoreCounts {
    match target {
        Target::DryRun(view) => {
            let ledger = Ledger::from_drawers(&view.drawers());
            execute(plan_store(export, store_id, &ledger), view, None, update).await
        }
        Target::Write(sink) => {
            let ledger = Ledger::from_drawers(&sink.drawers());
            execute(
                plan_store(export, store_id, &ledger),
                sink,
                Some(sink),
                update,
            )
            .await
        }
    }
}

/// The terminal status implied by a store's counts.
pub fn status_for(c: &StoreCounts, dry_run: bool) -> StoreStatus {
    if c.failed_writes > 0 {
        StoreStatus::Partial
    } else if c.new_memories + c.updated + c.new_triples == 0 {
        StoreStatus::UpToDate
    } else if dry_run {
        StoreStatus::WouldImport
    } else {
        StoreStatus::Imported
    }
}

/// The palace a store imports into; see the module doc for the rule.
fn target_palace(
    store: &DiscoveredStore,
    explicit: Option<&str>,
) -> Result<String, KuzuImportError> {
    if let Some(p) = explicit {
        if trusty_common::palace_id::palace_id_is_valid(p) {
            return Ok(p.to_string());
        }
        return Err(KuzuImportError::PalaceResolve(format!(
            "invalid palace id {p:?}"
        )));
    }
    trusty_common::palace_resolve::resolve_palace(store.project_dir())
        .map(|r| r.id)
        .map_err(|e| KuzuImportError::PalaceResolve(e.to_string()))
}

/// Open (creating if absent) `palace` for writing, refusing a snapshot open.
fn open_palace_for_write(
    data_root: &Path,
    palace: &str,
) -> Result<Arc<PalaceHandle>, KuzuImportError> {
    let registry = PalaceRegistry::new();
    let id = PalaceId::new(palace);
    let handle = match registry.open_palace(data_root, &id) {
        Ok(h) => h,
        Err(_) => registry
            .create_palace(
                data_root,
                Palace {
                    id: id.clone(),
                    name: palace.to_string(),
                    description: Some("Imported from kuzu-memory".to_string()),
                    created_at: chrono::Utc::now(),
                    data_dir: data_root.join(palace),
                },
            )
            .map_err(|e| KuzuImportError::Palace(format!("{e:#}")))?,
    };
    if handle.is_read_only() {
        return Err(KuzuImportError::PalaceLocked(palace.to_string()));
    }
    Ok(handle)
}

/// Read `palace`'s drawers and KG without opening a handle; empty when absent.
///
/// Test: `snapshot_view_of_a_missing_palace_is_empty_and_creates_nothing`.
pub fn open_snapshot_view(data_root: &Path, palace: &str) -> Result<SnapshotView, KuzuImportError> {
    let dir = data_root.join(palace);
    if !dir.join("kg.redb").exists() {
        return Ok(SnapshotView {
            drawers: Vec::new(),
            kg: None,
        });
    }
    let err = |e: anyhow::Error| KuzuImportError::Palace(format!("{e:#}"));
    let kg = KnowledgeGraph::open_with_intent(&dir.join("kg.db"), OpenIntent::ReadOnlyClient)
        .map_err(err)?;
    let drawers = kg.load_drawers().map_err(err)?;
    Ok(SnapshotView {
        drawers,
        kg: Some(kg),
    })
}

fn print_report(r: &StoreReport) {
    let c = &r.counts;
    let label = match &r.status {
        StoreStatus::Imported => "imported".green(),
        StoreStatus::WouldImport => "would import".cyan(),
        StoreStatus::UpToDate => "up to date".green(),
        StoreStatus::Empty => "empty".dimmed(),
        StoreStatus::Partial => "partial".yellow(),
        StoreStatus::Failed(_) => "failed".red(),
    };
    let palace = r.palace.as_deref().unwrap_or("?");
    println!(
        "[{label}] {} -> {palace}: memories {} (new {}, unchanged {}, changed {}, updated {}), \
         edges {} (new triples {}, existing {}, dangling {}), failed {}",
        r.store.display(),
        c.memories,
        c.new_memories,
        c.unchanged,
        c.changed,
        c.updated,
        c.edges,
        c.new_triples,
        c.existing_triples,
        c.dangling_edges,
        c.failed_writes
    );
    if let StoreStatus::Failed(e) = &r.status {
        println!("    {e}");
    }
}

fn print_totals(reports: &[StoreReport], update: bool) {
    let sum = |f: fn(&StoreCounts) -> usize| reports.iter().map(|r| f(&r.counts)).sum::<usize>();
    let failed = reports
        .iter()
        .filter(|r| matches!(r.status, StoreStatus::Failed(_)))
        .count();
    println!(
        "\n{} stores: {} memories, {} edges; new {}, unchanged {}, changed {}, updated {}, \
         new triples {}; {} store(s) failed",
        reports.len(),
        sum(|c| c.memories),
        sum(|c| c.edges),
        sum(|c| c.new_memories),
        sum(|c| c.unchanged),
        sum(|c| c.changed),
        sum(|c| c.updated),
        sum(|c| c.new_triples),
        failed
    );
    let changed = sum(|c| c.changed);
    if changed > 0 && !update {
        println!("{changed} memory(ies) changed in kuzu since import; re-run with --update to rewrite them.");
    }
}
