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
//! project dir is the parent of `.kuzu-memory`: a committed palace pin, then
//! the git `owner/repo` slug, then the `parent/dir` slug. A walk
//! (`--discover` / `--root`) refuses to run while `TRUSTY_MEMORY_PALACE` is
//! set, because that variable would send every store into one palace (see
//! [`palace_io::check_env_override`]); `--from` honours it. `--palace`
//! overrides the rule and is accepted only with `--from`. Every store line
//! prints the palace and the rule that chose it.
//!
//! **Identity and idempotency.** Every imported drawer carries
//! `source:kuzu-memory/<Memory.id>`, `kuzu-hash:<hash>` and the provenance tag
//! `kuzu-store:<store dir>`. A re-run skips a memory whose hash matches — from
//! whichever store, so a moved or re-cloned store imports nothing new — flags
//! one whose hash differs as changed, and rewrites that same drawer only under
//! `--update`. When another store that still exists holds a different memory
//! under the same id, the memory is skipped and its id reported. A triple is
//! asserted only when the exact `(subject, predicate, object)` is not already
//! active. There is no separate "done" record, so a run that fails part-way
//! can never be mistaken for a finished one: the next run imports the rest.
//!
//! **KG shape.** Entity `e` -> `entity:<e.id>` with `has_name` and
//! `entity_type` triples; MENTIONS -> `drawer:<uuid> mentions entity:<id>`;
//! RELATES_TO -> `drawer:<a> relates_to:<relationship_type> drawer:<b>`.
//!
//! **Writes need the daemon stopped.** The import opens palaces in this
//! process, so a real run refuses up front while a trusty-memory daemon is
//! running. `--dry-run` writes nothing: it reads each palace from a copy of
//! its `kg.redb` in a temp directory, so it may run beside a live daemon. A
//! dry run plans each store against the palace as it stands, so a `Memory.id`
//! two stores in one run share is counted new in both.
//!
//! Test: `kuzu_import::tests`.

pub mod apply;
pub mod bridge;
pub mod discovery;
pub mod ledger;
#[cfg(test)]
mod live_tests;
pub mod mapping;
pub mod palace_io;
pub mod report;
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::time::Duration;

use colored::Colorize;

use apply::{execute, plan_store, HandleSink, PalaceSink, PalaceView, StoreCounts};
use bridge::{export_store, resolve_python, CommandRunner, KuzuExport, SystemRunner};
use discovery::{discover, resolve_from, DiscoveredStore};
use ledger::Ledger;
use palace_io::{
    check_env_override, open_palace_for_write, open_snapshot_view, store_is_live, target_palace,
    DaemonProbe, SystemDaemonProbe,
};
use report::{print_report, print_totals};

/// Default walk depth under each root, matching `find -maxdepth 5`.
pub const DEFAULT_MAX_DEPTH: usize = 5;
/// Default bound on one store's export bridge, in seconds.
pub const DEFAULT_BRIDGE_TIMEOUT_SECS: u64 = 1800;

/// Everything that can stop one store's import, or the whole run.
///
/// Why: each arm is a distinct operator action (install kuzu-memory, stop the
/// daemon, upgrade the store), so each gets its own variant and message.
#[derive(Debug, thiserror::Error)]
pub enum KuzuImportError {
    #[error("kuzu-memory interpreter not found: {0}")]
    InterpreterNotFound(String),
    #[error("export bridge exited with status {code:?}: {stderr}")]
    BridgeFailed { code: Option<i32>, stderr: String },
    #[error("export bridge did not finish and was {0}; raise --bridge-timeout-secs")]
    BridgeTimedOut(String),
    #[error("export output is malformed: {0}")]
    MalformedExport(String),
    #[error("store lacks required Memory columns: {}", .0.join(", "))]
    SchemaColumnsMissing(Vec<String>),
    #[error("{} is not a kuzu-memory store (expected a .kuzu-memory dir or memories.db)", .0.display())]
    NotAStore(PathBuf),
    #[error("could not resolve a palace: {0}")]
    PalaceResolve(String),
    #[error(
        "TRUSTY_MEMORY_PALACE is set ({0:?}), and a --discover/--root walk would send every \
         store into that one palace; unset it for this run \
         (`env -u TRUSTY_MEMORY_PALACE trusty-memory import kuzu ...`) or import one store with --from"
    )]
    EnvPalaceWithWalk(String),
    #[error(
        "the trusty-memory daemon is running ({0}); stop the trusty-memory daemon first \
         (`trusty-memory stop`), then re-run the import (a --dry-run needs no stop)"
    )]
    DaemonRunning(String),
    #[error(
        "palace '{0}' is locked by the running daemon; stop it (`trusty-memory stop`) and re-run"
    )]
    PalaceLocked(String),
    #[error(
        "palace '{0}' has unreadable drawer rows, so what it already holds is unknown; \
         refusing to import into it until it is repaired"
    )]
    DrawersUnreadable(String),
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
            Self::BridgeTimedOut(_) => "bridge_timed_out",
            Self::MalformedExport(_) => "malformed_export",
            Self::SchemaColumnsMissing(_) => "schema_columns_missing",
            Self::NotAStore(_) => "not_a_store",
            Self::PalaceResolve(_) => "palace_resolve",
            Self::EnvPalaceWithWalk(_) => "env_palace_with_walk",
            Self::DaemonRunning(_) => "daemon_running",
            Self::PalaceLocked(_) => "palace_locked",
            Self::DrawersUnreadable(_) => "drawers_unreadable",
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
#[derive(Debug, Clone, clap::Args)]
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
    /// Kill one store's export bridge after this many seconds.
    #[arg(long, value_name = "SECS", default_value_t = DEFAULT_BRIDGE_TIMEOUT_SECS)]
    pub bridge_timeout_secs: u64,
}

impl Default for KuzuImportArgs {
    fn default() -> Self {
        Self {
            discover: false,
            root: Vec::new(),
            from: None,
            palace: None,
            dry_run: false,
            update: false,
            max_depth: DEFAULT_MAX_DEPTH,
            python: None,
            bridge_timeout_secs: DEFAULT_BRIDGE_TIMEOUT_SECS,
        }
    }
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
    /// The rule that chose `palace` (`--palace`, `pin file`, ...).
    pub source: Option<&'static str>,
    pub counts: StoreCounts,
    pub status: StoreStatus,
    /// The palace flush error after a write, when there was one (#277 L6).
    pub flush_error: Option<String>,
}

impl StoreReport {
    /// Whether this store makes the run exit non-zero.
    pub fn is_bad(&self) -> bool {
        matches!(self.status, StoreStatus::Failed(_) | StoreStatus::Partial)
            || self.flush_error.is_some()
    }
}

/// Where a store's plan is applied.
pub enum Target<'a> {
    /// Count only; nothing is written.
    DryRun(&'a dyn PalaceView),
    Write(&'a dyn PalaceSink),
}

/// What a run reads from outside itself; injectable for tests.
pub struct ImportEnv<'a> {
    pub runner: &'a dyn CommandRunner,
    pub daemon: &'a dyn DaemonProbe,
    pub python: &'a Path,
    pub data_root: &'a Path,
    /// The `TRUSTY_MEMORY_PALACE` value, when set.
    pub env_palace: Option<String>,
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
/// What: select stores, resolve the interpreter once, then [`run_import`].
/// Exits non-zero when any store failed, was only partly written, or failed
/// its flush; a changed-but-not-updated memory is a notice, not a failure.
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
    let runner = SystemRunner::new(Duration::from_secs(args.bridge_timeout_secs));
    let env = ImportEnv {
        runner: &runner,
        daemon: &SystemDaemonProbe,
        python: &python,
        data_root: &data_root,
        env_palace: trusty_common::palace_id::palace_override_from_env(),
    };
    let reports = run_import(&args, &stores, &env).await?;
    print_totals(&reports, args.update);
    let bad = reports.iter().filter(|r| r.is_bad()).count();
    if bad > 0 {
        anyhow::bail!(
            "{bad} of {} store(s) did not import completely",
            reports.len()
        );
    }
    Ok(())
}

/// Check the run may start, then import each store in turn.
///
/// What: refuses a walk while `TRUSTY_MEMORY_PALACE` is set (#277 H2), and a
/// real run while a daemon is live (#277), both before any store is read.
/// Test: `walk_refuses_an_env_palace_and_from_reports_its_source`,
/// `live_daemon_is_refused_before_any_store_is_read`.
pub async fn run_import(
    args: &KuzuImportArgs,
    stores: &[DiscoveredStore],
    env: &ImportEnv<'_>,
) -> Result<Vec<StoreReport>, KuzuImportError> {
    check_env_override(args.from.is_none(), env.env_palace.as_deref())?;
    if args.dry_run {
        println!("{} Dry run — nothing will be written.", "·".dimmed());
    } else if let Some(daemon) = env.daemon.live_daemon().await {
        return Err(KuzuImportError::DaemonRunning(daemon));
    }
    let mut reports = Vec::new();
    for store in stores {
        let report = import_one(env, store, args).await;
        print_report(&report, args.update);
        reports.push(report);
    }
    Ok(reports)
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
    env: &ImportEnv<'_>,
    store: &DiscoveredStore,
    args: &KuzuImportArgs,
) -> StoreReport {
    let mut report = StoreReport {
        store: store.dir.clone(),
        palace: None,
        source: None,
        counts: StoreCounts::default(),
        status: StoreStatus::Empty,
        flush_error: None,
    };
    let fetched = fetch(env.runner, env.python, store).and_then(|export| {
        let target = target_palace(store, args.palace.as_deref())?;
        Ok((export, target))
    });
    let (export, target) = match fetched {
        Ok(v) => v,
        Err(e) => {
            report.status = StoreStatus::Failed(e);
            return report;
        }
    };
    report.palace = Some(target.palace.clone());
    report.source = Some(target.source);
    if is_empty(&export) {
        return report;
    }
    let store_tag = store.dir.to_string_lossy();
    let counts = if args.dry_run {
        match open_snapshot_view(env.data_root, &target.palace) {
            Ok(view) => run_plan(&export, &store_tag, Target::DryRun(&view), args.update).await,
            Err(e) => {
                report.status = StoreStatus::Failed(e);
                return report;
            }
        }
    } else {
        match open_palace_for_write(env.data_root, &target.palace) {
            Ok(handle) => {
                let sink = HandleSink { handle };
                let counts = run_plan(&export, &store_tag, Target::Write(&sink), args.update).await;
                // #277 L6: a flush failure is the operator's to see.
                if let Err(e) = sink.handle.flush() {
                    report.flush_error = Some(format!("{e:#}"));
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

/// Plan `export` (read from the store at `store`) against `target`'s current
/// state and apply it.
///
/// Test: `import_twice_is_idempotent_on_palace_state`.
pub async fn run_plan(
    export: &KuzuExport,
    store: &str,
    target: Target<'_>,
    update: bool,
) -> StoreCounts {
    match target {
        Target::DryRun(view) => {
            let ledger = Ledger::from_drawers(&view.drawers());
            let plan = plan_store(export, store, &ledger, &store_is_live);
            execute(plan, view, None, update).await
        }
        Target::Write(sink) => {
            let ledger = Ledger::from_drawers(&sink.drawers());
            let plan = plan_store(export, store, &ledger, &store_is_live);
            execute(plan, sink, Some(sink), update).await
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
