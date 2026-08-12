//! Resolve a declared semantic lane to the facet of the same repo that holds
//! its vectors (#5069).
//!
//! Why: #5060 registers every `tm` session worktree `skip_vector`, and #5068
//! turned a pinned `stage: semantic` against such an index into a permanent
//! `503`. #5579 then registered the repo's base checkout with the vector lane
//! on — but nothing sent the query there, so semantic search was unreachable
//! from any worktree even once the vectors existed.
//!
//! What: on that refusal only, find the sibling index carrying the same
//! `PersistedIndex::repo_identity` whose vector lane can actually answer, and
//! hand it back so `search_handler` runs the caller's own query against it.
//!
//! Scope, deliberately narrow. This resolves WHERE a lane the caller DECLARED
//! runs; it never substitutes one lane for another, and it never fires for an
//! undeclared query — an unpinned search still degrades to lexical exactly as
//! before. That is the line ADR-0046 draws between resolving a request and
//! inferring what the caller meant, and it is why cross-repo routing is
//! refused outright: `repo_identity` is the whole reason the base checkout is
//! a facet of the caller's repo rather than an unrelated index.
//!
//! Test: `pinned_semantic_on_a_worktree_routes_to_the_base_facet`,
//! `semantic_routing_does_not_cross_repo_identities`,
//! `an_unpinned_query_is_not_routed_to_the_base_facet`.

use std::sync::Arc;

use crate::core::registry::{IndexHandle, IndexId};
use crate::service::persistence::PersistedIndex;

use super::state::SearchAppState;

/// How many sibling facets to probe before giving up.
///
/// A repo has one vector-carrying facet in the shape #5579 registers, so this
/// bounds a pathological registry (many facets, all unloadable) rather than a
/// real one — each probe can drive a cold load.
const MAX_FACET_CANDIDATES: usize = 4;

/// Find the facet of `requested`'s repo that can serve a semantic query.
///
/// Why: see the module doc. Called only after
/// [`super::degraded::vector_lane_unavailable`] has already refused the
/// requested index, so the cost here is paid on an error path, never on a
/// query the target could answer itself.
/// What: reads the persisted registry (honouring
/// [`SearchAppState::registry_path_override`], so tests need no env mutation),
/// takes `requested`'s `repo_identity`, and returns the first sibling — by id,
/// for determinism — that shares it, was built with the vector component
/// enabled, loads, has a readable corpus, and whose vector lane is Ready.
/// `None` when the requested index has no stored identity, when no sibling
/// qualifies, or when the registry cannot be read: every one of those leaves
/// the caller's original `503` intact.
/// Test: `pinned_semantic_on_a_worktree_routes_to_the_base_facet`,
/// `semantic_routing_does_not_cross_repo_identities`.
pub(super) async fn resolve_semantic_facet(
    state: &Arc<SearchAppState>,
    requested: &IndexId,
) -> Option<(IndexId, Arc<IndexHandle>)> {
    let entries = match &state.registry_path_override {
        Some(path) => crate::service::persistence::load_index_registry_at(path),
        None => crate::service::persistence::load_index_registry(),
    }
    .ok()?;

    let identity = entries
        .iter()
        .find(|e| e.id == requested.0)?
        .repo_identity
        .clone()?;

    let mut candidates: Vec<&PersistedIndex> = entries
        .iter()
        .filter(|e| {
            e.id != requested.0
                && !e.skip_vector
                && e.repo_identity.as_deref() == Some(identity.as_str())
        })
        .collect();
    candidates.sort_by(|a, b| a.id.cmp(&b.id));

    for entry in candidates.into_iter().take(MAX_FACET_CANDIDATES) {
        let id = IndexId::new(entry.id.clone());
        let Ok(handle) = super::index_resolve::resolve_or_load_index(state, &id).await else {
            continue;
        };
        // #4087: a facet whose durable corpus failed to open holds no chunks,
        // so routing to it would answer with an empty result set instead of the
        // refusal the caller can act on.
        if super::degraded::corpus_failure_response(&id.0, &handle)
            .await
            .is_some()
        {
            continue;
        }
        let stages = handle.stages.read().await.clone();
        if super::degraded::vector_lane_unavailable(&id.0, handle.skip_vector, &stages).is_none() {
            return Some((id, handle));
        }
    }
    None
}
