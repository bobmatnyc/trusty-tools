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
//! tree that was just written, runs one `trusty_okf::feed` pass over the tree
//! with this machine's index state (#10746 S12), and returns the JSON block the tool embeds as
//! `"index"`. It NEVER fails the ingest: a missing binding, an undiscoverable
//! daemon, and a failed push all degrade to a report carrying `pending` plus a
//! `reason`, because the tree write is the durable record and a later run
//! reconciles the backlog.
//!
//! Test: `super::tests::docstore_tool_reports_the_unsearchable_backlog`,
//! `super::tests::bound_store_reports_pending_when_the_daemon_is_down`,
//! `super::tests::sources_tool_reports_the_unsearchable_backlog`.

use crate::kb::okg::index_state::IndexStateRef;
use crate::kb::okg::registry::SourceSpec;
use crate::kb::store::KbStore;
use serde_json::{Value, json};

use crate::stores::okg_feed::{feed_source, unfed};

/// Feed everything this tree owes the agent's bound search index.
///
/// Why/What/Test: see the module doc. `agent` is the tool's own `agent`
/// argument — the hint that avoids searching the roster; when it is absent the
/// binding is resolved by finding the single agent that claims this tree.
/// #10746 S12: the pass runs in `trusty_okf::feed` with this machine's index
/// state; the report keeps its shape and adds the pass `status`.
pub(super) async fn feed_bound_index(
    store: &KbStore,
    spec: &SourceSpec,
    agent: Option<&str>,
) -> Value {
    let report = match resolve_feed(store, agent) {
        Ok((state, feed)) => feed_source(store, spec, &state, &feed).await,
        Err((reason, state)) => unfed(store, spec, state.as_ref(), reason),
    };
    serde_json::to_value(&report).unwrap_or_else(|e| json!({ "error": e.to_string() }))
}

/// Resolve the bound index's machine-local state and a live feed for it.
///
/// Every failure is a REASON string, never an error the caller propagates —
/// "this tree has no bound index" and "the search daemon is down" are normal,
/// reportable states. The state, when it resolved, still sizes the backlog.
fn resolve_feed(
    store: &KbStore,
    agent: Option<&str>,
) -> Result<(IndexStateRef, trusty_okf::feed::RpcIndexFeed), (String, Option<IndexStateRef>)> {
    let bound = crate::stores::bound_index_for_tree(
        &crate::agents::agents_dir_candidates(),
        &super::knowledge_dir(),
        &store.root,
        agent,
    )
    .map_err(|reason| (reason, None))?;
    let state = IndexStateRef::machine_local(&bound.index).map_err(|e| {
        (
            format!("machine-local index state directory unavailable: {e:#}"),
            None,
        )
    })?;
    let socket = match trusty_common::search_rpc::search_socket() {
        Ok(s) => s,
        Err(e) => {
            return Err((
                format!("trusty-search socket unavailable: {e}"),
                Some(state),
            ));
        }
    };
    Ok((state, trusty_okf::feed::RpcIndexFeed::new(socket)))
}
