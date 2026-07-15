//! Bounded-concurrency cache warming, deduped on the extracted ticket KEY.
//!
//! Why: code-critic HIGH finding on issue #2719 / PR #2720 — deduping the
//! concurrent external-source pass by raw commit *message* does not protect
//! the resolver's per-TICKET cache. Two differently-worded commits that
//! reference the same ticket key produce two distinct messages; each message
//! is resolved by its own concurrent task, and [`super::ExternalSourceResolver::resolve`]'s
//! check→fetch→populate sequence is non-atomic (the cache lock is dropped
//! across the network `.await`). Under concurrency both tasks can observe a
//! cache miss for the SAME key before either populates it, firing up to
//! `concurrency` duplicate fetches for one hot ticket — on a rate-limited
//! JIRA instance a throttled fetch returns `None`, silently losing the
//! classification signal.
//!
//! What: [`warm_cache`] extracts and dedupes ticket keys/refs across a whole
//! batch of messages PER SOURCE — before any fetch is dispatched — then
//! fetches only the still-uncached keys with a bounded `buffer_unordered`.
//! Because dedup happens on the same key space the cache is keyed by (not on
//! message text), every unique ticket is fetched at most once regardless of
//! how many different messages reference it. Callers ([`super::super::pipeline_external`])
//! call this once to fully warm every source's cache before issuing any
//! per-message `resolve` calls, so those calls are guaranteed cache hits and
//! race-free by construction.
//!
//! Test: `resolver_tests::warm_cache_*` plus the wiremock integration tests
//! in `classify::pipeline_tests` (`pipeline_external_sources_dedupe_and_apply`,
//! `pipeline_external_sources_dedupe_distinct_messages_same_ticket`).

use std::collections::HashSet;
use std::future::Future;
use std::sync::Mutex;

use futures::stream::StreamExt;

use super::{confluence, datadog, github_issues, jira, linear, shortcut};
use super::{Cache, ExternalSignal, ExternalSourceResolver, GitHubRef, SourceState};

impl ExternalSourceResolver {
    /// Pre-resolve every ticket key referenced across `messages`, for every
    /// configured source, with bounded concurrency.
    ///
    /// Why: this is the single entry point that closes the #2719 HIGH race —
    /// see the module doc. Calling this before any [`Self::resolve`] on the
    /// same `messages` guarantees those calls hit a warm cache.
    /// What: iterates sources in order, warming each independently (dedup and
    /// concurrency bound apply per source; sources themselves are warmed
    /// sequentially, which is fine — cache population, unlike the original
    /// serial network loop, is what actually costs wall-clock time and that
    /// is now bounded-concurrent within each source).
    /// Test: `resolver_tests::warm_cache_dedupes_ticket_key_across_distinct_messages`.
    pub async fn warm_cache(&self, messages: &[String], concurrency: usize) {
        let concurrency = concurrency.max(1);
        for state in &self.sources {
            warm_source(&self.client, state, messages, concurrency).await;
        }
    }
}

/// Warm a single source's cache from `messages`.
///
/// Why: isolates the per-source-type extraction/filter logic (which mirrors
/// [`ExternalSourceResolver::resolve_source`]'s existing per-variant match arms)
/// from the generic dedupe-then-fetch mechanics in [`warm_generic`].
/// What: for each `SourceState` variant, extracts + filters candidate
/// keys/refs across all of `messages`, dedupes by the same key the source's
/// cache uses, then delegates to [`warm_generic`].
/// Test: exercised via [`ExternalSourceResolver::warm_cache`]'s tests.
async fn warm_source(
    client: &reqwest::Client,
    state: &SourceState,
    messages: &[String],
    concurrency: usize,
) {
    match state {
        SourceState::Jira {
            config,
            cache,
            base_url_override,
        } => {
            let mut seen = HashSet::new();
            let keys: Vec<String> = messages
                .iter()
                .flat_map(|m| jira::extract_jira_keys(m))
                .filter(|k| {
                    config.project_keys.is_empty()
                        || config
                            .project_keys
                            .iter()
                            .any(|pk| k.starts_with(&format!("{pk}-")))
                })
                .filter(|k| seen.insert(k.clone()))
                .collect();
            let base_url_override = base_url_override.as_deref();
            warm_generic(
                cache,
                concurrency,
                keys,
                |k: &String| k.clone(),
                |k: String| async move {
                    let mut result = jira::fetch_issues_batch(
                        client,
                        config,
                        std::slice::from_ref(&k),
                        base_url_override,
                    )
                    .await;
                    result.remove(&k).flatten()
                },
            )
            .await;
        }

        SourceState::GithubIssues {
            config,
            cache,
            api_base_override,
        } => {
            let mut seen = HashSet::new();
            let refs: Vec<GitHubRef> = messages
                .iter()
                .flat_map(|m| github_issues::extract_github_refs(m))
                .filter(|r| {
                    let repo = r.repo.as_deref().unwrap_or(config.repo.as_str());
                    seen.insert(format!("{repo}#{}", r.number))
                })
                .collect();
            let api_base_override = api_base_override.as_deref();
            warm_generic(
                cache,
                concurrency,
                refs,
                |r: &GitHubRef| {
                    let repo = r.repo.as_deref().unwrap_or(config.repo.as_str());
                    format!("{repo}#{}", r.number)
                },
                |r: GitHubRef| async move {
                    let repo = r.repo.clone().unwrap_or_else(|| config.repo.clone());
                    let key = format!("{repo}#{}", r.number);
                    let mut result = github_issues::fetch_issues_batch(
                        client,
                        config,
                        std::slice::from_ref(&r),
                        api_base_override,
                    )
                    .await;
                    result.remove(&key).flatten()
                },
            )
            .await;
        }

        SourceState::Linear {
            config,
            cache,
            api_base_override,
        } => {
            let mut seen = HashSet::new();
            let keys: Vec<String> = messages
                .iter()
                .flat_map(|m| linear::extract_linear_keys(m))
                .filter(|k| linear::matches_team_key(k, &config.team_keys))
                .filter(|k| seen.insert(k.clone()))
                .collect();
            let api_base_override = api_base_override.as_deref();
            warm_generic(
                cache,
                concurrency,
                keys,
                |k: &String| k.clone(),
                |k: String| async move {
                    let mut result = linear::fetch_issues_batch(
                        client,
                        config,
                        std::slice::from_ref(&k),
                        api_base_override,
                    )
                    .await;
                    result.remove(&k).flatten()
                },
            )
            .await;
        }

        SourceState::Shortcut {
            config,
            cache,
            api_base_override,
        } => {
            let mut seen = HashSet::new();
            let ids: Vec<u64> = messages
                .iter()
                .flat_map(|m| shortcut::extract_shortcut_ids(m))
                .filter(|id| seen.insert(*id))
                .collect();
            let api_base_override = api_base_override.as_deref();
            warm_generic(
                cache,
                concurrency,
                ids,
                |id: &u64| id.to_string(),
                |id: u64| async move {
                    let key = id.to_string();
                    let mut result = shortcut::fetch_stories_batch(
                        client,
                        config,
                        std::slice::from_ref(&id),
                        api_base_override,
                    )
                    .await;
                    result.remove(&key).flatten()
                },
            )
            .await;
        }

        SourceState::Confluence {
            config,
            cache,
            api_base_override,
        } => {
            let mut seen = HashSet::new();
            let ids: Vec<u64> = messages
                .iter()
                .flat_map(|m| confluence::extract_confluence_ids(m))
                .filter(|id| seen.insert(*id))
                .collect();
            let api_base_override = api_base_override.as_deref();
            warm_generic(
                cache,
                concurrency,
                ids,
                |id: &u64| id.to_string(),
                |id: u64| async move {
                    let key = id.to_string();
                    let mut result = confluence::fetch_pages_batch(
                        client,
                        config,
                        std::slice::from_ref(&id),
                        api_base_override,
                    )
                    .await;
                    result.remove(&key).flatten()
                },
            )
            .await;
        }

        SourceState::Datadog {
            config,
            cache,
            api_base_override,
        } => {
            let mut seen = HashSet::new();
            let shas: Vec<String> = messages
                .iter()
                .flat_map(|m| datadog::extract_commit_shas(m))
                .filter(|s| seen.insert(s.clone()))
                .collect();
            let api_base_override = api_base_override.as_deref();
            warm_generic(
                cache,
                concurrency,
                shas,
                |s: &String| s.clone(),
                |s: String| async move {
                    let mut result = datadog::check_shas_batch(
                        client,
                        config,
                        std::slice::from_ref(&s),
                        api_base_override,
                    )
                    .await;
                    result.remove(&s).flatten()
                },
            )
            .await;
        }
    }
}

/// Generic dedupe-checked, bounded-concurrency single-item cache warmer.
///
/// Why: the six source variants differ only in their key type and fetch
/// call; factoring the "skip already-cached, fetch the rest with
/// `buffer_unordered`, populate the cache" mechanics once here keeps
/// [`warm_source`] to source-specific extraction/filtering only.
/// What: filters `items` down to cache misses (checked once, up front, so no
/// wasted fetch dispatch for already-warm keys), fetches each miss via
/// `fetch_one` with at most `concurrency` in flight, and inserts every
/// `(key, signal)` result into `cache` — including `None` results, matching
/// the existing per-source cache semantics (a `None` entry means "checked,
/// no signal", not "not yet checked").
/// Test: `resolver_tests::warm_cache_*`.
async fn warm_generic<T, K, F, Fut>(
    cache: &Mutex<Cache>,
    concurrency: usize,
    items: Vec<T>,
    key_of: K,
    fetch_one: F,
) where
    K: Fn(&T) -> String,
    F: Fn(T) -> Fut,
    Fut: Future<Output = Option<ExternalSignal>>,
{
    if items.is_empty() {
        return;
    }
    let misses: Vec<T> = {
        let guard = cache.lock().expect("cache lock");
        items
            .into_iter()
            .filter(|it| !guard.contains_key(&key_of(it)))
            .collect()
    };
    if misses.is_empty() {
        return;
    }

    let key_of = &key_of;
    let fetch_one = &fetch_one;
    let fetched: Vec<(String, Option<ExternalSignal>)> =
        futures::stream::iter(misses.into_iter().map(|item| async move {
            let key = key_of(&item);
            let signal = fetch_one(item).await;
            (key, signal)
        }))
        .buffer_unordered(concurrency)
        .collect()
        .await;

    let mut guard = cache.lock().expect("cache lock");
    for (k, sig) in fetched {
        guard.insert(k, sig);
    }
}

#[cfg(test)]
#[path = "warm_tests.rs"]
mod warm_tests;
