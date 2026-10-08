//! Handler for `trusty-search reindex` (bare reindex, no force/verify).

use super::explicit_target::{
    classify_explicit_target, resolve_explicit_target, with_source, ExplicitTarget, IndexIdSource,
    ParkedTargets,
};
use super::index_resolve::{print_index_header, resolve_index};
use super::reindex_engine::run_reindex_opts;
use crate::detect::detect_project;
use anyhow::{bail, Result};

/// Why: a reindex POSTs `root_path` with the id, so a wrong id rebases that
/// index onto PATH and overwrites its corpus (#8737). The id and the root must
/// therefore come from the same explicit source.
/// What: resolves the target with the #8175 rule — `TRUSTY_INDEX` alone
/// refuses before any network call; PATH and/or `-i` resolve through the
/// daemon (PATH plus id must agree), and the index's REGISTERED root is sent.
/// With neither, the CWD-detected id and root are used as before. Then drives
/// `run_reindex_opts`, which renders the progress stream. #9214: the daemon is
/// reached over its socket only, started on the indexing device (issue #24)
/// when nothing answers.
/// Test: `tests/reindex_quantize_env_conflict_8737.rs`.
///
/// `timeout` is the user-supplied `--timeout` value: `None` means
/// progress-aware stall detection; `Some(n)` means hard cap at n seconds.
pub async fn handle_reindex(
    explicit_index: &Option<String>,
    index_source: Option<IndexIdSource>,
    path: Option<std::path::PathBuf>,
    timeout: Option<u64>,
) -> Result<()> {
    let target = classify_explicit_target(
        "reindex",
        path,
        with_source(explicit_index.clone(), index_source),
    );
    if let ExplicitTarget::Refuse(reason) = &target {
        bail!("{reason}");
    }
    let (index_id, reindex_path) = if target == ExplicitTarget::CwdAutoDetect {
        let (index_id, warned) = resolve_index(&None)?;
        print_index_header(&index_id, warned);
        super::daemon_rpc::connect_for_indexing().await?;
        // #6550: detection can refuse the resolved root.
        let cwd = std::env::current_dir().unwrap_or_default();
        (index_id, detect_project(&cwd)?.root_path)
    } else {
        // #9214: the socket only; nothing answering is an error naming it.
        let client = super::daemon_rpc::connect_for_indexing().await?;
        // #8737: any failure here (down, not found, unavailable, mismatch)
        // refuses before the reindex kickoff is ever sent.
        let Some((id, root, via)) =
            resolve_explicit_target("reindex", &client, target, ParkedTargets::Refuse).await?
        else {
            bail!("reindex target resolution returned no index");
        };
        println!(
            "Resolving target: index \"{id}\" ({}), via {via}",
            root.display()
        );
        (id, root)
    };
    let (timeout_secs, timeout_explicit) = match timeout {
        Some(n) => (n, true),
        None => (0, false),
    };
    run_reindex_opts(&index_id, &reindex_path, timeout_secs, timeout_explicit).await
}
