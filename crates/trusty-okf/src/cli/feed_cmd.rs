//! The `feed` verb and `import run --feed` (SPEC-OKF-01 §4.10 phase 3,
//! §5.4). Feature `search-feed`.
//!
//! What: dials `--socket` (or a test's fake), runs one feed pass, then
//! reads the live index identity from `search.index.status` and each
//! touched concept's index state from `--state-dir`. An identity that
//! cannot be read is `Unreadable`, so every record reads Pending (§4.9.2
//! rule 4). The `feed` verb exits through `feed::exit_code` (criterion 51).
//! Test: `c51_cli_feed_verb_exits_non_zero_unless_searchable`,
//! `c135_identity_and_no_d1_paths_never_read_searchable`.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::json;

use super::import_cmd::{bundle_root, fail};
use super::{Args, Ctx, Fail};
use crate::bundle::Bundle;
use crate::feed::{self, FeedReport, IndexFeed, RpcIndexFeed, live_identity as identity_of};
use crate::import::ImportReport;
use crate::state::{IndexStateDir, LiveIdentity};

fn runtime() -> Result<tokio::runtime::Runtime, Fail> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| Fail::Code(1, format!("cannot start the async runtime: {e}")))
}

/// Run `f` with the context's fake, or a socket feed on `--socket`.
fn with_sink<T>(a: &Args, ctx: &Ctx<'_>, f: impl FnOnce(&dyn IndexFeed) -> T) -> T {
    match ctx.sink {
        Some(s) => f(s),
        None => {
            let socket = PathBuf::from(a.one("--socket").unwrap_or_default());
            f(&RpcIndexFeed::new(socket))
        }
    }
}

async fn read_live(sink: &dyn IndexFeed, index: &str) -> LiveIdentity {
    match sink
        .call("search.index.status", json!({ "index_id": index }))
        .await
    {
        Ok(reply) => identity_of(&reply),
        Err(e) => LiveIdentity::Unreadable(e.to_string()),
    }
}

/// The live identity for `import status`; `Unreadable` without `--socket`.
pub(crate) fn live_identity(a: &Args, ctx: &Ctx<'_>) -> LiveIdentity {
    let Some(index) = a.one("--index") else {
        return LiveIdentity::Unreadable("no --index".into());
    };
    if ctx.sink.is_none() && a.one("--socket").is_none() {
        return LiveIdentity::Unreadable("no --socket; the live identity was not read".into());
    }
    match runtime() {
        Ok(rt) => with_sink(a, ctx, |s| rt.block_on(read_live(s, index))),
        Err(_) => LiveIdentity::Unreadable("no async runtime".into()),
    }
}

/// One feed pass after an import, then the report's index states. Returns
/// the pass error, if any; the report then keeps the states it reads.
pub(crate) fn after_import(
    a: &Args,
    root: &Path,
    report: &mut ImportReport,
    ctx: &Ctx<'_>,
) -> Result<Option<String>, Fail> {
    let index = a.one("--index").unwrap_or_default();
    let state = IndexStateDir::new(a.one("--state-dir").unwrap_or_default());
    let bundle = Bundle::open(root).map(|(b, _)| b).map_err(|e| fail(&e))?;
    let rt = runtime()?;
    let (pass, live) = with_sink(a, ctx, |s| {
        rt.block_on(async {
            let pass = feed::feed(&bundle, &state, index, s).await;
            (pass, read_live(s, index).await)
        })
    });
    report
        .apply_index_states(&bundle, &state, index, &live)
        .map_err(|e| fail(&e))?;
    Ok(pass.err().map(|e| e.to_string()))
}

/// `feed <bundle-root> --index ID --state-dir DIR --socket PATH`.
pub(crate) fn feed(a: &Args, out: &mut dyn Write, ctx: &Ctx<'_>) -> Result<i32, Fail> {
    a.only(&["--index", "--state-dir", "--socket", "--json"])
        .map_err(Fail::Usage)?;
    let root = bundle_root(a, 1)?;
    for k in ["--index", "--state-dir", "--socket"] {
        if a.one(k).is_none() {
            return Err(Fail::Usage(format!("feed needs {k}")));
        }
    }
    let index = a.one("--index").unwrap_or_default();
    let state = IndexStateDir::new(a.one("--state-dir").unwrap_or_default());
    let bundle = Bundle::open(&root).map(|(b, _)| b).map_err(|e| fail(&e))?;
    let rt = runtime()?;
    let result = with_sink(a, ctx, |s| {
        rt.block_on(feed::feed(&bundle, &state, index, s))
    });
    let code = feed::exit_code(&result);
    match &result {
        Ok(r) => print(out, r, a.has("--json")),
        Err(e) => return Err(Fail::Code(code, e.to_string())),
    }
    Ok(code)
}

fn print(out: &mut dyn Write, r: &FeedReport, as_json: bool) {
    if as_json {
        let text = serde_json::to_string_pretty(r).unwrap_or_default();
        let _ = writeln!(out, "{text}");
        return;
    }
    let _ = writeln!(
        out,
        "feed: status {:?}, acknowledged {}, searchable {}, removed {}, pending {}",
        r.status, r.acknowledged, r.searchable, r.removed, r.pending
    );
    for e in &r.errors {
        let _ = writeln!(out, "error {}: {}", e.path, e.message);
    }
}
