//! The one entry point for asserting a caller-supplied KG triple.
//!
//! Why: writing a triple that a caller chose is three obligations, not one —
//! the Tier S admission gate (#4888), the write itself, and refreshing the
//! always-injected prompt cache when the predicate is hot. Six surfaces owed
//! all three and each carried its own copy, so the third drifted: the HTTP
//! `POST /api/v1/palaces/{id}/kg` path (#5524) and the chat `kg_assert` tool
//! (#4905) stored the triple and never refreshed, which made a standing rule
//! written through either surface invisible to every later turn while both
//! reported success. Which client a user happened to use silently decided
//! whether their fact took effect. Centralising the sequence here removes the
//! call-site obligation that the drift was made of — a new write surface gets
//! all three by construction, and a behaviour fix lands once.
//!
//! What: [`assert_triple`] runs admission → assert → refresh in that order and
//! returns [`KgWriteError`] on any step. The admission guard is held across the
//! write, which is what makes the Tier S check-then-write sequence atomic.
//! Refresh failures are reported, never swallowed: a triple that reached
//! storage without reaching the prompt surface is precisely the defect this
//! module exists to prevent, so it is a distinct error variant
//! ([`KgWriteError::CacheRefresh`]) rather than a `warn!`.
//!
//! Test: `kg_write_refreshes_cache_for_hot_predicate`,
//! `kg_write_skips_refresh_for_cold_predicate`,
//! `kg_write_admission_refusal_leaves_storage_and_cache_untouched`,
//! `kg_write_batch_policy_defers_refresh` in the inline `tests` module; the
//! per-surface proofs are `http_kg_assert_endpoint_refreshes_prompt_cache`
//! (`web::tests::prompt_tests`) and `chat_kg_assert_refreshes_prompt_cache`
//! (`web::tests::chat_tests`).

use std::sync::Arc;

use trusty_common::memory_core::store::kg::Triple;
use trusty_common::memory_core::PalaceHandle;

use crate::AppState;

/// Why a caller-supplied KG assert did not complete.
///
/// Why: the three failure points have genuinely different meanings to a
/// caller. `Admission` means nothing was written and the caller should fix the
/// request; `Assert` means nothing was written and the daemon is at fault;
/// `CacheRefresh` means the triple IS in storage but the always-injected
/// surface is stale. Collapsing them into one opaque error would leave HTTP
/// unable to choose 400 vs 500, and would let the third be mistaken for a
/// clean success — which is the bug in #5524 and #4905.
/// What: a `thiserror` enum over `anyhow::Error`. `Admission` is transparent so
/// the Tier S refusal text (which names the occupants and the tool that
/// retires one) reaches the caller unchanged.
/// Test: `kg_write_admission_refusal_leaves_storage_and_cache_untouched`.
#[derive(Debug, thiserror::Error)]
pub enum KgWriteError {
    /// The Tier S gate refused the write. Nothing was stored.
    #[error(transparent)]
    Admission(anyhow::Error),
    /// The KG write itself failed. Nothing was stored.
    #[error("kg assert: {0:#}")]
    Assert(anyhow::Error),
    /// The triple was stored but the prompt cache could not be rebuilt, so the
    /// fact is not yet on the always-injected surface.
    #[error("triple written but prompt cache refresh failed: {0:#}")]
    CacheRefresh(anyhow::Error),
}

/// When [`assert_triple`] refreshes the prompt cache after a hot write.
///
/// Why: a rebuild walks every registered palace, so a caller asserting N
/// triples in one pass would pay N full walks for one net change. `Batch` lets
/// such a caller defer to a single trailing refresh WITHOUT deciding for itself
/// what counts as hot — that decision stays in [`assert_triple`] either way,
/// which is the property this module exists to protect.
/// What: `Inline` refreshes before returning; `Batch` skips the refresh and
/// reports hotness in the receipt for [`refresh_after_batch`].
/// Test: `kg_write_batch_policy_defers_refresh`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CachePolicy {
    /// Refresh inline when the predicate is hot. Every single-write caller.
    Inline,
    /// Defer the refresh; the caller must pass the accumulated hotness to
    /// [`refresh_after_batch`] when its loop ends.
    Batch,
}

/// What [`assert_triple`] did, for callers that batch.
#[derive(Debug, Clone, Copy)]
pub struct AssertReceipt {
    /// Whether the asserted predicate is on the Tier S prompt surface.
    pub hot: bool,
}

/// Assert `triple` into `handle`'s KG, keeping the Tier S surface coherent.
///
/// Why: see the module doc — this is the single sequence every caller-supplied
/// KG write must run, and the reason it is a function rather than a convention
/// is that the convention already failed twice (#5524, #4905).
///
/// What: three steps in a fixed order.
/// 1. [`crate::prompt_facts::check_tier_s_admission`], whose returned guard is
///    held until after the write — dropping it earlier reopens the
///    check-then-write race the lock closes.
/// 2. `KnowledgeGraph::assert`.
/// 3. When the predicate is hot AND `policy` is [`CachePolicy::Inline`],
///    [`crate::prompt_facts::rebuild_prompt_cache`].
///
/// Hotness is read BEFORE the write because `triple` is moved into it. A
/// failure at any step is returned; step 3 in particular is never downgraded to
/// a log line, because "stored but invisible" is the exact user-visible symptom
/// of the two issues this replaces.
///
/// Test: `kg_write_refreshes_cache_for_hot_predicate`,
/// `kg_write_skips_refresh_for_cold_predicate`,
/// `kg_write_admission_refusal_leaves_storage_and_cache_untouched`.
pub async fn assert_triple(
    state: &AppState,
    handle: &Arc<PalaceHandle>,
    triple: Triple,
    policy: CachePolicy,
) -> Result<AssertReceipt, KgWriteError> {
    // #5524: `_admission` holds the Tier S lock across `kg.assert` below.
    let _admission = crate::prompt_facts::check_tier_s_admission(
        state,
        handle,
        &triple.subject,
        &triple.predicate,
        &triple.object,
    )
    .await
    .map_err(KgWriteError::Admission)?;

    // Read before the move — `assert` consumes `triple`.
    let hot = crate::prompt_facts::is_hot_predicate(&triple.predicate);
    handle
        .kg
        .assert(triple)
        .await
        .map_err(KgWriteError::Assert)?;

    if hot && policy == CachePolicy::Inline {
        refresh(state).await?;
    }
    Ok(AssertReceipt { hot })
}

/// Refresh the prompt cache after a [`CachePolicy::Batch`] loop.
///
/// Why: gives a batching caller the trailing half of [`assert_triple`] without
/// letting it re-derive "was any of that hot?" from its own predicate list.
/// What: no-op when `any_hot` is false; otherwise the same refresh
/// [`assert_triple`] performs inline.
/// Test: `kg_write_batch_policy_defers_refresh`.
pub async fn refresh_after_batch(state: &AppState, any_hot: bool) -> Result<(), KgWriteError> {
    if any_hot {
        refresh(state).await
    } else {
        Ok(())
    }
}

/// The single call to [`crate::prompt_facts::rebuild_prompt_cache`].
///
/// Note: this arm is not reachable today — `gather_hot_facts` logs and skips a
/// palace it cannot read and then returns `Ok`, so the rebuild has no fallible
/// step left. That skip silently truncates the cache (and the Tier S occupancy
/// count that gates admission), which is a separate defect; the error is
/// propagated here so that fixing it turns this into a reported failure rather
/// than a silent one.
async fn refresh(state: &AppState) -> Result<(), KgWriteError> {
    crate::prompt_facts::rebuild_prompt_cache(state)
        .await
        .map_err(KgWriteError::CacheRefresh)
}

#[cfg(test)]
mod tests {
    use super::*;
    use trusty_common::memory_core::palace::PalaceId;

    /// Build an `AppState` over a fresh temp root with one created palace.
    fn state_with_palace(name: &str) -> (AppState, Arc<PalaceHandle>) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let state = AppState::new(root).with_default_palace(Some(name.to_string()));
        let palace = trusty_common::memory_core::Palace {
            id: PalaceId::new(name),
            name: name.to_string(),
            description: None,
            created_at: chrono::Utc::now(),
            data_dir: state.data_root.join(name),
        };
        let handle = state
            .registry
            .create_palace(&state.data_root, palace)
            .expect("create palace");
        (state, handle)
    }

    fn triple(subject: &str, predicate: &str, object: &str) -> Triple {
        Triple {
            subject: subject.to_string(),
            predicate: predicate.to_string(),
            object: object.to_string(),
            valid_from: chrono::Utc::now(),
            valid_to: None,
            confidence: 1.0,
            provenance: Some("test".to_string()),
        }
    }

    /// The whole point of the module: a hot write reaches the prompt cache
    /// without the caller doing anything beyond calling `assert_triple`.
    #[tokio::test]
    async fn kg_write_refreshes_cache_for_hot_predicate() {
        let (state, handle) = state_with_palace("hotwrite");
        let receipt = assert_triple(
            &state,
            &handle,
            triple("rust", "has_convention", "no unwrap in library code"),
            CachePolicy::Inline,
        )
        .await
        .expect("assert");
        assert!(receipt.hot);

        let guard = state.prompt_context_cache.read().await;
        assert!(
            guard.formatted.contains("no unwrap in library code"),
            "hot write missing from cache; got: {}",
            guard.formatted
        );
    }

    /// A cold predicate must not pay for a rebuild — the cache stays empty
    /// rather than merely unchanged, proving the refresh was skipped and not
    /// just uninformative.
    #[tokio::test]
    async fn kg_write_skips_refresh_for_cold_predicate() {
        let (state, handle) = state_with_palace("coldwrite");
        let receipt = assert_triple(
            &state,
            &handle,
            triple("alice", "works_at", "Acme"),
            CachePolicy::Inline,
        )
        .await
        .expect("assert");
        assert!(!receipt.hot);

        let guard = state.prompt_context_cache.read().await;
        assert!(
            guard.triples.is_empty(),
            "cold write should not populate the cache; got: {:?}",
            guard.triples
        );
    }

    /// Error arm: a refused write must leave BOTH storage and the cache
    /// untouched. A gate that rejects while the row lands is the failure this
    /// whole family keeps reproducing.
    #[tokio::test]
    async fn kg_write_admission_refusal_leaves_storage_and_cache_untouched() {
        let (state, handle) = state_with_palace("refusal");
        let over_long = "x".repeat(crate::prompt_facts::TIER_S_MAX_OBJECT_CHARS + 1);
        let err = assert_triple(
            &state,
            &handle,
            triple("subj", "has_convention", &over_long),
            CachePolicy::Inline,
        )
        .await
        .expect_err("over-long object must be refused");
        assert!(
            matches!(err, KgWriteError::Admission(_)),
            "expected Admission, got: {err:?}"
        );

        let stored = handle.kg.query_active("subj").await.expect("query");
        assert!(
            stored.is_empty(),
            "refused write reached storage: {stored:?}"
        );
        let guard = state.prompt_context_cache.read().await;
        assert!(guard.triples.is_empty(), "refused write reached the cache");
    }

    /// `Batch` defers the refresh to `refresh_after_batch`, and the receipt is
    /// what tells the caller a refresh is owed.
    #[tokio::test]
    async fn kg_write_batch_policy_defers_refresh() {
        let (state, handle) = state_with_palace("batchwrite");
        let receipt = assert_triple(
            &state,
            &handle,
            triple("tm", "is_alias_for", "trusty-memory"),
            CachePolicy::Batch,
        )
        .await
        .expect("assert");
        assert!(receipt.hot);
        assert!(
            state.prompt_context_cache.read().await.triples.is_empty(),
            "Batch must not refresh inline"
        );

        refresh_after_batch(&state, receipt.hot)
            .await
            .expect("batch refresh");
        let guard = state.prompt_context_cache.read().await;
        assert!(
            guard.formatted.contains("tm → trusty-memory"),
            "batch refresh missed the write; got: {}",
            guard.formatted
        );
    }

    /// `refresh_after_batch(_, false)` must not touch the cache — otherwise a
    /// cold-only batch would pay for a full registry walk.
    #[tokio::test]
    async fn kg_write_batch_refresh_is_a_noop_when_nothing_was_hot() {
        let (state, handle) = state_with_palace("batchcold");
        assert_triple(
            &state,
            &handle,
            triple("bob", "lives_in", "Paris"),
            CachePolicy::Batch,
        )
        .await
        .expect("assert");
        refresh_after_batch(&state, false).await.expect("noop");
        assert!(state.prompt_context_cache.read().await.triples.is_empty());
    }
}
