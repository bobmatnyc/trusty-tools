//! The ingest→index step shared by every `okg_ingest_*` tool (#3892).
//!
//! Why: an ingest that writes the tree and stops there is the bug — the entities
//! are real, the report is truthful, and `vector_search` still returns nothing.
//! Every ingest tool therefore ends with the same three questions: which index
//! does this tree feed, is the search daemon reachable, and how much is still
//! not searchable? Answering them in one place means a future connector inherits
//! the behaviour with no per-connector work (DOC-55 SPEC-OKGIMPORT-06 §7.2.3).
//!
//! What: [`feed_bound_index`] resolves the agent's `[[stores]]` binding for the
//! tree that was just written, drains that source's index backlog through the
//! live trusty-search feed, and returns the JSON block the tool embeds as
//! `"index"`. It NEVER fails the ingest: a missing binding, an undiscoverable
//! daemon, and a failed push all degrade to a report carrying `pending` plus a
//! `reason`, because the tree write is the durable record and a later run
//! reconciles the backlog.
//!
//! Test: `super::tests::docstore_tool_reports_the_unsearchable_backlog`,
//! `super::tests::bound_store_reports_pending_when_the_daemon_is_down`,
//! `super::tests::sources_tool_reports_the_unsearchable_backlog`.

use serde_json::{Value, json};
use trusty_kb::okg::registry::SourceSpec;
use trusty_kb::store::KbStore;

use crate::stores::index_feed::{HttpIndexFeed, IndexFeed, IndexFeedReport, feed_source};

/// Feed everything this source owes the agent's bound search index.
///
/// Why/What/Test: see the module doc. `agent` is the tool's own `agent`
/// argument — the hint that avoids searching the roster; when it is absent the
/// binding is resolved by finding the single agent that claims this tree.
pub(super) async fn feed_bound_index(
    store: &KbStore,
    spec: &SourceSpec,
    agent: Option<&str>,
    now: &str,
) -> Value {
    let report = match resolve_feed(store, agent) {
        Ok((index, feed)) => match feed_source(store, spec, &index, feed.as_ref(), now).await {
            Ok(report) => report,
            // A reconcile that cannot even read its own journal is worth
            // reporting, but it is still not an ingest failure.
            Err(e) => IndexFeedReport::not_fed(format!("index reconcile failed: {e}"), 0, 0),
        },
        Err(reason) => {
            let (pending, synced) = backlog_counts(store, spec);
            IndexFeedReport::not_fed(reason, pending, synced)
        }
    };
    serde_json::to_value(&report).unwrap_or_else(|e| json!({ "error": e.to_string() }))
}

/// Resolve the bound index id and a live feed for it.
///
/// Every failure is a REASON string, never an error the caller propagates —
/// "this tree has no bound index" and "the search daemon is down" are normal,
/// reportable states.
fn resolve_feed(
    store: &KbStore,
    agent: Option<&str>,
) -> Result<(String, Box<dyn IndexFeed>), String> {
    let bound = crate::stores::bound_index_for_tree(
        &crate::agents::agents_dir_candidates(),
        &super::knowledge_dir(),
        &store.root,
        agent,
    )?;
    let Some(base) = trusty_common::resolve_daemon_base_url("trusty-search") else {
        return Err(format!(
            "trusty-search daemon not discoverable (no address file; is it running?) — \
             `{}` is bound but nothing was pushed",
            bound.index
        ));
    };
    let feed = HttpIndexFeed::new(base).map_err(|e| format!("HTTP client unavailable: {e}"))?;
    Ok((bound.index, Box::new(feed)))
}

/// `(pending, synced)` for a source whose feed could not run.
///
/// Why: reporting "not fed" without the size of the backlog tells the caller
/// nothing actionable. A reconcile failure here degrades to zeroes rather than
/// masking the real reason with a second one.
fn backlog_counts(store: &KbStore, spec: &SourceSpec) -> (usize, usize) {
    match store.okg_index_backlog(spec) {
        Ok(b) => (b.tasks.len() + b.notes.len(), b.synced),
        Err(_) => (0, 0),
    }
}
