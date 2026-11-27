//! Re-apply the carryover window's live-corpus writes to staging (#12064).
//!
//! Why: see `core::indexer::carryover_journal`. A write or a delete that
//! committed to the live corpus after the carryover snapshot is not in
//! staging; the promotion would drop the write and bring the deleted rows
//! back.
//! What: [`reapply_window_writes`] runs under the indexer write guard, after
//! the copy and before the swap. For each journaled file, prefix and chunk
//! id it replaces staging's rows with live's current rows. A failure removes
//! the staging file and is returned, so the run aborts with the live corpus,
//! which holds the write, still attached; the runner marks the reindex
//! failed.
//! Test: `a_write_and_a_delete_in_the_carryover_window_survive_the_promotion`,
//! `a_failed_reapply_aborts_the_run_and_keeps_the_write_in_live`.

use std::path::Path;
use std::sync::Arc;

use anyhow::Context;

use crate::core::corpus::CorpusStore;
use crate::core::indexer::carryover_journal::JournalKeys;

/// #12064 test seam: index ids whose re-apply fails.
#[cfg(test)]
pub(super) static TEST_FAIL_REAPPLY: std::sync::Mutex<Vec<String>> =
    std::sync::Mutex::new(Vec::new());

/// Copy live's current rows for `keys` into `staging`; see the module doc.
pub(super) async fn reapply_window_writes(
    index_id: &str,
    keys: JournalKeys,
    live: Option<Arc<CorpusStore>>,
    staging: &Arc<CorpusStore>,
    staging_path: &Path,
) -> anyhow::Result<()> {
    let Some(live) = live.filter(|_| !keys.is_empty()) else {
        return Ok(());
    };
    #[cfg(test)]
    let fail = TEST_FAIL_REAPPLY
        .lock()
        .is_ok_and(|ids| ids.iter().any(|i| i == index_id));
    let staging = Arc::clone(staging);
    let (files, prefixes, ids) = (keys.files.len(), keys.prefixes.len(), keys.chunk_ids.len());
    let copied = tokio::task::spawn_blocking(move || {
        #[cfg(test)]
        if fail {
            anyhow::bail!("injected carryover re-apply failure (#12064 test seam)");
        }
        let mut copied = 0;
        for sel in keys.selections() {
            copied += staging.resync_selection_from(&live, &sel)?;
        }
        let ids: Vec<String> = keys.chunk_ids.into_iter().collect();
        Ok(copied + staging.resync_chunk_ids_from(&live, &ids)?)
    })
    .await
    .context("carryover re-apply task did not complete")
    .and_then(|r| r);
    match copied {
        Ok(copied) => {
            tracing::info!(
                index_id,
                files,
                prefixes,
                chunk_ids = ids,
                rows_copied = copied,
                "re-applied the live-corpus writes made during the carryover copy (#12064)"
            );
            Ok(())
        }
        Err(e) => {
            tracing::error!(
                index_id,
                files,
                prefixes,
                chunk_ids = ids,
                error = %format!("{e:#}"),
                "carryover window writes not re-applied; aborting the staged run (#12064)"
            );
            if let Err(rm) = std::fs::remove_file(staging_path) {
                tracing::warn!(index_id, error = %rm, "staging corpus not removed (#12064)");
            }
            Err(e.context(format!(
                "reindex[{index_id}]: writes made during the carryover copy were not re-applied \
                 to staging"
            )))
        }
    }
}
