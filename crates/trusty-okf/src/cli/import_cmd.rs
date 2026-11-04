//! `import plan`, `import run` and `import status` (SPEC-OKF-01 §4.10).
//!
//! What: [`store_for`] turns `--from dir:<path>` into the store row: an
//! existing doc-store row for the same canonical directory keeps its ID and
//! flags; a new row is `import-<slug of the directory name>` (or `--store`)
//! with `tombstone_deleted` set, so a vanished file is tombstoned on a
//! deletion-safe run. A store ID that already names another source is
//! refused. Every refusal here happens before any write (exit 2).
//! Test: `crates/trusty-okf/tests/cli_criteria.rs`.

use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{DateTime, FixedOffset};

use super::{Args, Ctx, Fail, output};
use crate::OkfError;
use crate::build::policy::DocStorePolicy;
use crate::build::registry::{Locator, SourceRegistry, SourceSpec};
use crate::bundle::Bundle;
use crate::extract::{Extractor, NoWorker};
use crate::format::concept::slugify;
use crate::import::{
    DocTreeSource, ImportPlan, ImportSpec, import, import_status, is_refusal, plan_import_with,
};
use crate::state::{IndexStateDir, LiveIdentity};

const IMPORT_OPTS: &[&str] = &[
    "--from",
    "--store",
    "--include",
    "--exclude",
    "--json",
    "--worker",
];
const RUN_OPTS: &[&str] = &[
    "--from",
    "--store",
    "--include",
    "--exclude",
    "--json",
    "--dry-run",
    "--feed",
    "--index",
    "--state-dir",
    "--socket",
    "--now",
    "--worker",
];

/// A library error as a CLI failure, with its §4.10 code: 2 for a
/// refusal before any write, else 1.
pub(crate) fn fail(e: &OkfError) -> Fail {
    Fail::Code(if is_refusal(e) { 2 } else { 1 }, e.to_string())
}

/// The bundle root named by the third positional argument; it must exist.
pub(crate) fn bundle_root(a: &Args, at: usize) -> Result<PathBuf, Fail> {
    let root = a
        .pos
        .get(at)
        .ok_or_else(|| Fail::Usage("name the bundle root".into()))?;
    if a.pos.len() > at + 1 {
        return Err(Fail::Usage(format!(
            "unexpected argument {}",
            a.pos[at + 1]
        )));
    }
    let root = PathBuf::from(root);
    if !root.is_dir() {
        return Err(Fail::Code(
            2,
            format!("bundle root {} is not a directory", root.display()),
        ));
    }
    Ok(root)
}

fn open(root: &Path) -> Result<Bundle, Fail> {
    Bundle::open(root).map(|(b, _)| b).map_err(|e| fail(&e))
}

fn now(a: &Args) -> Result<DateTime<FixedOffset>, Fail> {
    match a.one("--now") {
        Some(t) => {
            DateTime::parse_from_rfc3339(t).map_err(|e| Fail::Usage(format!("--now {t}: {e}")))
        }
        None => Ok(chrono::Utc::now().fixed_offset()),
    }
}

/// The store row for `--from dir:<path>` (see the module doc).
fn store_for(a: &Args, root: &Path, now: &str) -> Result<(SourceSpec, PathBuf), Fail> {
    let from = a
        .one("--from")
        .ok_or_else(|| Fail::Usage("name the source with --from dir:<path>".into()))?;
    let dir = from.strip_prefix("dir:").ok_or_else(|| {
        Fail::Code(
            2,
            format!("--from {from}: this build imports only dir:<path> sources"),
        )
    })?;
    let dir = Path::new(dir)
        .canonicalize()
        .map_err(|e| Fail::Code(2, format!("source {dir}: {e}")))?;
    let path = dir.to_string_lossy().into_owned();
    let registry = SourceRegistry::load(root).map_err(|e| fail(&e))?;
    let same =
        |s: &&SourceSpec| matches!(&s.locator, Locator::DocStore { path: p, .. } if *p == path);
    if let Some(row) = registry.sources.iter().find(same) {
        if a.one("--store").is_some_and(|s| slugify(s) != row.id) {
            return Err(Fail::Code(2, format!("{path} is already store {}", row.id)));
        }
        return Ok((row.clone(), dir));
    }
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let id = a
        .one("--store")
        .map_or_else(|| format!("import-{}", slugify(&name)), str::to_string);
    if registry.get(&id).is_some() {
        return Err(Fail::Code(
            2,
            format!(
                "store {} already names another source; pass --store",
                slugify(&id)
            ),
        ));
    }
    let locator = Locator::DocStore {
        path,
        extensions: Vec::new(),
        recursive: true,
    };
    let mut row = SourceSpec::new(&id, None, locator, now);
    row.tombstone_deleted = true;
    Ok((row, dir))
}

/// The run's extractor for items the built-in tier does not handle:
/// the worker `--worker` names, else [`NoWorker`] (`worker_absent`).
///
/// #10746 S10: the worker is found only at the given path, never through
/// `PATH` (§2.16 "Absent worker"); a missing file is `worker_absent`, shown
/// by the plan. Working directories go under `/tmp`, a fixed path, since
/// the CLI reads no environment variable (B3). Without the
/// `extract-worker` feature `--worker` is refused before any write (2).
fn extractor(a: &Args, ctx: &Ctx<'_>) -> Result<Box<dyn Extractor>, Fail> {
    let Some(path) = a.one("--worker") else {
        return Ok(Box::new(NoWorker));
    };
    #[cfg(feature = "extract-worker")]
    {
        use crate::extract::worker::{WorkerConfig, WorkerExtractor};
        let cfg = WorkerConfig::new(path, "/tmp");
        let mut w = WorkerExtractor::new(cfg).map_err(|e| Fail::Code(2, e))?;
        if ctx.catch_interrupt {
            w = w.with_cancel(interrupt::watch().map_err(|e| Fail::Code(2, e))?);
        }
        Ok(Box::new(w))
    }
    #[cfg(not(feature = "extract-worker"))]
    {
        // No worker client in this build, so SIGINT keeps its default.
        let _ = (path, ctx.catch_interrupt);
        Err(Fail::Code(
            2,
            "this build has no extract-worker feature; --worker is not available".into(),
        ))
    }
}

/// A context that leaves SIGINT alone (`import plan` writes nothing).
fn no_interrupt() -> Ctx<'static> {
    Ctx {
        #[cfg(feature = "search-feed")]
        sink: None,
        catch_interrupt: false,
        _p: std::marker::PhantomData,
    }
}

/// #10746 S10 fix: SIGINT becomes the worker run's cancel flag.
#[cfg(feature = "extract-worker")]
mod interrupt {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use nix::sys::signal::{SigSet, Signal};

    /// Block SIGINT in this thread (threads it starts later inherit the
    /// mask) and start a thread that waits for it and sets the flag. Without
    /// `unsafe`: no handler runs; `sigwait` returns the signal. Errors: the
    /// mask could not be set; the run is refused before any write, so
    /// Ctrl-C can never orphan a worker.
    pub(super) fn watch() -> Result<Arc<AtomicBool>, String> {
        let mut set = SigSet::empty();
        set.add(Signal::SIGINT);
        set.thread_block()
            .map_err(|e| format!("cannot take over SIGINT: {e}"))?;
        let flag = Arc::new(AtomicBool::new(false));
        let seen = Arc::clone(&flag);
        std::thread::spawn(move || {
            if set.wait() == Ok(Signal::SIGINT) {
                seen.store(true, Ordering::SeqCst);
            }
        });
        Ok(flag)
    }
}

/// The spec, the plan and the source for `import plan` and `import run`.
fn prepare(
    a: &Args,
    root: &Path,
    stamp: &str,
    ex: &dyn Extractor,
) -> Result<(Bundle, ImportPlan, DocTreeSource), Fail> {
    let bundle = open(root)?;
    let (store, dir) = store_for(a, root, stamp)?;
    let mut spec = ImportSpec::new(store);
    spec.include = a.all("--include");
    spec.exclude = a.all("--exclude");
    let policy = DocStorePolicy::new(vec![dir.clone()]);
    let mut src = DocTreeSource::open(&dir, &policy).map_err(|e| fail(&e))?;
    let plan = plan_import_with(&bundle, &spec, &mut src, ex).map_err(|e| fail(&e))?;
    Ok((bundle, plan, src))
}

/// `import plan`: print the plan; write nothing.
pub(crate) fn plan(a: &Args, out: &mut dyn Write) -> Result<i32, Fail> {
    a.only(IMPORT_OPTS).map_err(Fail::Usage)?;
    let root = bundle_root(a, 2)?;
    let ex = extractor(a, &no_interrupt())?;
    let (_, plan, _) = prepare(a, &root, "", ex.as_ref())?;
    output::plan(out, &plan, a.has("--json"));
    Ok(0)
}

/// `import run`: plan, apply, optionally feed, report.
pub(crate) fn run(a: &Args, out: &mut dyn Write, ctx: &Ctx<'_>) -> Result<i32, Fail> {
    a.only(RUN_OPTS).map_err(Fail::Usage)?;
    let root = bundle_root(a, 2)?;
    let feed = a.has("--feed");
    if feed || a.one("--index").is_some() {
        feed_args(a, &root)?;
    }
    let now = now(a)?;
    let stamp = crate::bundle::record::instant_string(&now);
    let ex = extractor(a, ctx)?;
    let (mut bundle, plan, mut src) = prepare(a, &root, &stamp, ex.as_ref())?;
    if a.has("--dry-run") {
        output::plan(out, &plan, a.has("--json"));
        return Ok(0);
    }
    let result = import(&mut bundle, &plan, &mut src, ex.as_ref(), now);
    let mut report = match result {
        Ok(r) => r,
        Err(e) => return Err(fail(&e)),
    };
    // #10746 S8 fix: the import has written. From here every failure prints
    // the report first and exits 1 (§4.10), never 2 ("refused before any
    // write").
    let feed_error = if feed {
        match feed_after_import(a, &root, &mut report, ctx) {
            Ok(e) => e,
            Err(Fail::Code(_, e) | Fail::Usage(e)) => Some(e),
        }
    } else {
        None
    };
    output::report(out, &report, a.has("--json"));
    match feed_error {
        Some(e) => Err(Fail::Code(
            1,
            format!("feed pass failed after the import: {e}"),
        )),
        None => Ok(report.exit_code),
    }
}

/// `--index`, `--state-dir` and `--socket` together, and a feed-capable
/// build; checked before any write. #10746 S8 fix: the state directory and
/// index ID are checked as the journal will read them (`StateDirInBundle`,
/// `PathEscape`), so a bad value refuses with 2 and no file changes.
fn feed_args(a: &Args, root: &Path) -> Result<(), Fail> {
    if !a.has("--feed") {
        return Err(Fail::Usage("--index needs --feed".into()));
    }
    for k in ["--index", "--state-dir", "--socket"] {
        if a.one(k).is_none() {
            return Err(Fail::Usage(format!("--feed needs {k}")));
        }
    }
    #[cfg(not(feature = "search-feed"))]
    {
        let _ = root;
        Err(super::no_feed())
    }
    #[cfg(feature = "search-feed")]
    {
        let state = IndexStateDir::new(a.one("--state-dir").unwrap_or_default());
        crate::state::IndexJournal::open(&state, root, a.one("--index").unwrap_or_default())
            .map(|_| ())
            .map_err(|e| fail(&e))
    }
}

#[cfg(feature = "search-feed")]
fn feed_after_import(
    a: &Args,
    root: &Path,
    report: &mut crate::import::ImportReport,
    ctx: &Ctx<'_>,
) -> Result<Option<String>, Fail> {
    super::feed_cmd::after_import(a, root, report, ctx)
}

#[cfg(not(feature = "search-feed"))]
fn feed_after_import(
    _a: &Args,
    _root: &Path,
    _report: &mut crate::import::ImportReport,
    _ctx: &Ctx<'_>,
) -> Result<Option<String>, Fail> {
    Err(super::no_feed())
}

/// `import status`: per-store, per-axis counts and the logged attempts.
pub(crate) fn status(a: &Args, out: &mut dyn Write, ctx: &Ctx<'_>) -> Result<i32, Fail> {
    a.only(&["--store", "--index", "--state-dir", "--socket", "--json"])
        .map_err(Fail::Usage)?;
    let root = bundle_root(a, 2)?;
    let bundle = open(&root)?;
    let index = match (a.one("--index"), a.one("--state-dir")) {
        (Some(i), Some(d)) => Some((i.to_string(), IndexStateDir::new(d))),
        (None, None) => None,
        _ => return Err(Fail::Usage("--index and --state-dir go together".into())),
    };
    let live = live_for_status(a, ctx);
    let idx = index.as_ref().map(|(i, d)| (d, i.as_str(), &live));
    let (stores, runs) = import_status(&bundle, a.one("--store"), idx).map_err(|e| fail(&e))?;
    output::status(out, &stores, &runs, a.has("--json"));
    Ok(0)
}

#[cfg(feature = "search-feed")]
fn live_for_status(a: &Args, ctx: &Ctx<'_>) -> LiveIdentity {
    super::feed_cmd::live_identity(a, ctx)
}

#[cfg(not(feature = "search-feed"))]
fn live_for_status(_a: &Args, _ctx: &Ctx<'_>) -> LiveIdentity {
    LiveIdentity::Unreadable("this build has no search-feed feature".into())
}
