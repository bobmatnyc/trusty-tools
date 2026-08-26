//! Draining and persisting the concurrent pull-request fetch.
//!
//! Why: the fetch fans out across providers on a `JoinSet` and every result has
//! four possible dispositions — the task panicked, no provider matched the
//! returned name, the fetch failed, or the store failed. Each maps to a
//! different [`crate::collect::CollectionFault`] severity, and keeping that
//! mapping in one place is what stops a half-persisted run from reporting
//! success. Extracted from [`crate::collect::collector`] for the same reason
//! [`crate::collect::github_pipeline`] was: that module is on a frozen SLOC
//! budget.
//!
//! What: [`drain_and_store_pull_requests`] awaits every spawned fetch, matches
//! each result back to the provider that produced it, and persists the batch.
//!
//! Test: `crate::collect::correlate::tests` covers what the persisted rows then
//! feed; the fault severities are covered by
//! `crate::commands::collect::tests::a_failed_stage_makes_collect_exit_non_zero`.

use std::sync::Arc;

use tokio::task::JoinSet;
use tracing::info;

use crate::collect::collector::CollectionStats;
use crate::collect::errors::Result;
use crate::collect::pr_provider::PrProvider;
use crate::core::db::Database;
use crate::core::models::PullRequest;

/// Await every spawned pull-request fetch and persist what came back.
///
/// Why: see the module header — one place decides which failures reach the
/// process exit code. A fetch or store that failed wholesale is
/// [`CollectionStats::fail_stage`], because that provider's data is absent from
/// the database; a payload anomaly that cost only some harvested detail is
/// [`CollectionStats::skip_item`].
///
/// What: drains `set`, matches each `(provider_name, result)` pair back to its
/// entry in `providers`, records an empty-source-branch anomaly (#5734), then
/// calls that provider's `store_pull_requests`. Every disposition is recorded;
/// none aborts the drain, so one bad provider cannot cost the others.
///
/// #5734: a provider that answers with `Some("")` for a head ref made a claim
/// and the claim was empty, which is an anomaly worth reporting. `None` means
/// the provider never claimed to supply one — Bitbucket today — and is silent
/// by design. Collapsing the two would make a broken payload indistinguishable
/// from a branch harvest that legitimately found nothing.
///
/// Test: `tests::blank_head_ref_is_recorded_as_a_skipped_item`.
pub(super) async fn drain_and_store_pull_requests(
    mut set: JoinSet<(String, Result<Vec<PullRequest>>)>,
    providers: &[Arc<dyn PrProvider + Send + Sync>],
    db: &mut Database,
    stats: &mut CollectionStats,
) {
    while let Some(joined) = set.join_next().await {
        let (provider_name, fetch_result) = match joined {
            Ok(t) => t,
            Err(e) => {
                stats.fail_stage(format!("PR fetch task panicked: {e}"));
                continue;
            }
        };
        let prs = match fetch_result {
            Ok(prs) => prs,
            Err(e) => {
                stats.fail_stage(format!("{provider_name} PR fetch failed: {e}"));
                continue;
            }
        };
        let Some(provider) = providers.iter().find(|p| p.name() == provider_name) else {
            stats.fail_stage(format!(
                "internal: no provider registered for '{provider_name}' when storing PRs"
            ));
            continue;
        };
        record_blank_head_refs(stats, &provider_name, &prs);
        // #6084: a walk that stopped at a cap returns rows that look complete.
        // Recording each notice is what keeps the shortfall visible.
        for notice in provider.fetch_notices() {
            stats.skip_item(format!("{provider_name}: {notice}"));
        }
        match provider.store_pull_requests(db, &prs) {
            Ok(n) => {
                info!(provider = %provider_name, prs = n, "stored pull requests");
                stats.prs_fetched += n;
            }
            Err(e) => {
                stats.fail_stage(format!("{provider_name} PR store failed: {e}"));
            }
        }
    }
}

/// Record pull requests whose provider claimed a source branch and gave an
/// empty one (#5734).
///
/// Why: without this the branch harvest fails open — a provider returning blank
/// refs yields zero keys, which looks exactly like a repository whose branches
/// carry no ticket keys.
/// What: counts `Some("")` head refs and records ONE `skip_item` naming the
/// count. `None` is skipped: that is "no claim made", not a fault.
/// Test: `tests::blank_head_ref_is_recorded_as_a_skipped_item`,
/// `tests::absent_head_ref_is_not_a_fault`.
fn record_blank_head_refs(stats: &mut CollectionStats, provider: &str, prs: &[PullRequest]) {
    let blank = prs
        .iter()
        .filter(|p| p.head_ref.as_deref() == Some(""))
        .count();
    if blank > 0 {
        stats.skip_item(format!(
            "{provider}: {blank} pull request(s) reported an empty source branch; \
             no branch ticket key harvested for them"
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::models::PrState;
    use chrono::Utc;

    fn pr(head_ref: Option<&str>) -> PullRequest {
        PullRequest {
            id: 0,
            pr_number: 1,
            repository: "acme/widgets".into(),
            title: "T".into(),
            author: "ada".into(),
            state: PrState::Merged,
            created_at: Utc::now(),
            merged_at: None,
            commit_shas: "[]".into(),
            fetched_at: "2026-01-01T00:00:00Z".into(),
            head_ref: head_ref.map(str::to_string),
            body_ticket_id: None,
        }
    }

    /// Why: #5734 — an empty source branch harvests nothing, and without a
    /// recorded fault that is indistinguishable from a repository whose
    /// branches simply carry no keys. This is the fail-open the check exists
    /// to close.
    /// What: two blank refs produce one `ItemSkipped` fault naming the count,
    /// and no stage failure — the rest of the batch still persisted.
    /// Test: this test itself.
    #[test]
    fn blank_head_ref_is_recorded_as_a_skipped_item() {
        let mut stats = CollectionStats::default();
        record_blank_head_refs(
            &mut stats,
            "github",
            &[pr(Some("")), pr(Some("feature/PROJ-1")), pr(Some(""))],
        );
        assert_eq!(
            stats.errors.len(),
            1,
            "one aggregated fault, not one per PR"
        );
        assert!(
            stats.stage_failures().is_empty(),
            "a payload anomaly must not reach the exit code"
        );
        let msg = stats.errors[0].message.clone();
        assert!(msg.contains("github"), "{msg}");
        assert!(msg.contains('2'), "the count must be named: {msg}");
    }

    /// Why: `None` means the provider never claimed to supply a source branch —
    /// Bitbucket today. Reporting that as a fault would make every Bitbucket
    /// collection noisy about a feature it does not implement.
    /// What: absent and non-empty head refs record nothing.
    /// Test: this test itself.
    #[test]
    fn absent_head_ref_is_not_a_fault() {
        let mut stats = CollectionStats::default();
        record_blank_head_refs(
            &mut stats,
            "bitbucket",
            &[pr(None), pr(None), pr(Some("feature/PROJ-1"))],
        );
        assert!(stats.errors.is_empty());
    }
}
