//! Bounded-concurrency external-source (JIRA / GitHub / …) resolution for the
//! classification pipeline.
//!
//! Why: prior to issue #2719 the pipeline resolved external tickets strictly
//! serially — one network round-trip per commit, each capable of taking up to
//! the 30s JIRA timeout — producing multi-minute "zero progress" stalls on
//! large corpora (~181k commits). This module fans the network work out with a
//! bounded `buffer_unordered` (mirroring the LLM fallback tier) while preserving
//! the serial loop's "resolve each ticket exactly once" throughput guarantee.
//!
//! What: [`resolve_external_signals`] collects the *unique* commit messages that
//! still need external resolution, resolves them concurrently (bounded), emits
//! progress, and returns a `message -> ExternalSignal` map the caller applies to
//! every commit sharing that message. [`resolve_unique`] is the generic,
//! HTTP-free core seam, unit-tested with a counting fake resolver.
//!
//! Test: `pipeline_external_tests` (dedupe + concurrency) and the wiremock
//! `pipeline_tests::pipeline_external_sources_*` integration tests.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};

use futures::stream::StreamExt;
use tracing::info;

use super::pipeline_db::CommitRow;
use super::sources::{ExternalSignal, ExternalSourceResolver};
use super::tiers::ClassificationResult;

/// Maximum number of external-source lookups in flight at once.
///
/// Why: external APIs (JIRA, GitHub) enforce rate limits, so unbounded fan-out
/// would trade a serial stall for a burst of throttled/failed requests. A modest
/// bound of 8 mirrors the LLM fallback tier's default concurrency and keeps the
/// HTTP budget proportional while collapsing the multi-minute serial stall.
/// What: the `buffer_unordered` bound used by [`resolve_external_signals`].
/// Test: exercised indirectly by the pipeline integration tests.
pub(super) const EXTERNAL_SOURCE_CONCURRENCY: usize = 8;

/// Emit an `info!` progress line every this many resolved unique messages.
///
/// Why: in a non-TTY context (the Duetto cron ETL) the indicatif progress bar is
/// hidden, so an explicit periodic log line to stderr is what makes a slow source
/// observable rather than a silent hang.
/// What: the modulus cadence for the progress `info!` in [`resolve_external_signals`].
/// Test: covered indirectly; log output is side-effect-only.
const PROGRESS_LOG_EVERY: usize = 50;

/// Resolve external-source signals for every commit lacking a Tier-0 override.
///
/// Why: this is the pipeline's Tier-0.5 entry point; concentrating the
/// dedupe-then-concurrent-resolve logic here keeps `pipeline.rs` thin and under
/// its SLOC budget.
/// What: collects the unique messages of override-free commits, resolves each
/// exactly once with bounded concurrency via [`resolve_unique`], emits progress
/// to stderr, and returns a `message -> ExternalSignal` map. Commits with a
/// Tier-0 manual override are excluded (overrides win). Returns an empty map
/// when nothing needs resolution.
/// Test: `pipeline_tests::pipeline_external_sources_dedupe_and_apply`.
pub(super) async fn resolve_external_signals(
    commits: &[CommitRow],
    overrides: &HashMap<i64, ClassificationResult>,
    resolver: &ExternalSourceResolver,
) -> HashMap<String, ExternalSignal> {
    // Dedupe BEFORE spawning: firing one future per commit would launch
    // duplicate in-flight requests for the same ticket; collapsing to unique
    // messages first preserves the serial loop's fetch-once guarantee.
    let unique = unique_unresolved_messages(commits, overrides);
    if unique.is_empty() {
        return HashMap::new();
    }

    let total = unique.len();
    let pb = super::pipeline_db::make_progress(total as u64, "External sources");
    let done = AtomicUsize::new(0);
    let pb_ref = &pb;
    let done_ref = &done;

    let map = resolve_unique(unique, EXTERNAL_SOURCE_CONCURRENCY, |message| async move {
        let signal = resolver.resolve(&message).await;
        pb_ref.inc(1);
        let n = done_ref.fetch_add(1, Ordering::Relaxed) + 1;
        if n.is_multiple_of(PROGRESS_LOG_EVERY) || n == total {
            info!(resolved = n, total, "external-source resolution progress");
        }
        signal
    })
    .await;

    pb.finish_and_clear();
    map
}

/// Collect the unique messages of commits that still need external resolution.
///
/// Why: dedupe must happen before fan-out so each referenced ticket is fetched
/// exactly once even under concurrency; this is the pure, HTTP-free core of that
/// step, kept separate so it can be unit-tested directly.
/// What: returns each distinct `message` (first-occurrence order) among commits
/// that do NOT carry a Tier-0 manual override. Override commits are excluded
/// because the override verdict wins regardless of any external signal.
/// Test: `pipeline_external_tests::unique_messages_dedupes_and_excludes_overrides`.
pub(super) fn unique_unresolved_messages(
    commits: &[CommitRow],
    overrides: &HashMap<i64, ClassificationResult>,
) -> Vec<String> {
    let mut seen: HashSet<&str> = HashSet::new();
    commits
        .iter()
        .filter(|c| !overrides.contains_key(&c.id))
        .filter(|c| seen.insert(c.message.as_str()))
        .map(|c| c.message.clone())
        .collect()
}

/// Resolve a set of unique messages concurrently, deduped, HTTP-free core.
///
/// Why: isolating the dedupe + bounded-concurrency + collect logic behind a
/// `resolve` closure gives a test seam that counts invocations without any
/// network I/O, proving each unique message is resolved exactly once.
/// What: maps each unique message through `resolve` with at most `concurrency`
/// futures in flight (`buffer_unordered`), then collects the non-`None` results
/// into a `message -> ExternalSignal` map. Input messages must already be unique;
/// callers dedupe upstream.
/// Test: `pipeline_external_tests::resolve_unique_invokes_once_per_message` and
/// `resolve_unique_bounds_concurrency`.
pub(super) async fn resolve_unique<F, Fut>(
    unique_messages: Vec<String>,
    concurrency: usize,
    resolve: F,
) -> HashMap<String, ExternalSignal>
where
    F: Fn(String) -> Fut,
    Fut: Future<Output = Option<ExternalSignal>>,
{
    let concurrency = concurrency.max(1);
    let resolve = &resolve;
    let pairs: Vec<(String, Option<ExternalSignal>)> =
        futures::stream::iter(unique_messages.into_iter().map(|message| async move {
            let signal = resolve(message.clone()).await;
            (message, signal)
        }))
        .buffer_unordered(concurrency)
        .collect()
        .await;

    pairs
        .into_iter()
        .filter_map(|(message, signal)| signal.map(|s| (message, s)))
        .collect()
}

#[cfg(test)]
#[path = "pipeline_external_tests.rs"]
mod pipeline_external_tests;
