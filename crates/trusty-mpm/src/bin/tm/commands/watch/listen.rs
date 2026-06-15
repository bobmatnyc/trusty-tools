//! The long-running `tm watch listen` poll loop with dedup + graceful SIGINT.
//!
//! Why: `listen` is the pragmatic, locally-testable form of "watch a board":
//! repeatedly poll for label-matched issues, dispatch any not seen before, and
//! sleep until the next cycle — running until Ctrl-C. Tracking already-processed
//! issue numbers in-memory prevents re-dispatching the same issue every cycle.
//! Webhook-based listen is a future enhancement (#follow-up: replace this poll
//! loop with a GitHub webhook receiver so dispatch is event-driven rather than
//! polled).
//! What: [`select_new_issues`] is the pure dedup step (returns the issues whose
//! numbers are not yet in the seen-set and records them); [`run_listen_loop`]
//! drives poll → select-new → dispatch → sleep over an [`IssueLister`], cancelling
//! cleanly on `Ctrl-C`. The seen-set lives in-memory for the process lifetime
//! (a persistent state file is a documented future enhancement).
//! Test: `select_new_issues_*` cover the dedup invariants; the loop's wiring is
//! exercised via the manual e2e (it sleeps + traps signals).

use std::collections::HashSet;

use trusty_mpm::runtime::RuntimeKind;

use super::args::ResolvedWatch;
use super::dispatch::{DispatchMode, dispatch_issue};
use super::github::{IssueLister, MatchedIssue};

/// Select the issues not yet seen, recording them in `seen`.
///
/// Why: the core of listen-mode dedup — each poll re-returns every matching
/// issue, but only issues whose numbers are new this process should be
/// dispatched, or the loop would re-run the same work every cycle. Keeping this
/// a pure function over an explicit set makes the invariant ("an issue is
/// dispatched at most once per process") directly testable.
/// What: returns the subset of `issues` whose `number` is absent from `seen`,
/// inserting each returned number into `seen` as it goes. Input order is
/// preserved. Dry-run callers pass a set they DON'T want mutated by using a
/// throwaway clone; execute callers pass the persistent set.
/// Test: `select_new_issues_filters_seen`, `select_new_issues_records_new`,
/// `select_new_issues_dedups_within_batch`.
pub(crate) fn select_new_issues(
    issues: &[MatchedIssue],
    seen: &mut HashSet<u64>,
) -> Vec<MatchedIssue> {
    let mut fresh = Vec::new();
    for issue in issues {
        if seen.insert(issue.number) {
            fresh.push(issue.clone());
        }
    }
    fresh
}

/// Decide whether the in-memory-dedup restart warning should be emitted.
///
/// Why: the `seen` set lives only for the process lifetime, so a restart loses
/// it and `--execute` would re-dispatch every currently-open matching issue. That
/// risk only exists when we are actually spawning work, so the warning must fire
/// for `Execute` and stay silent for `DryRun` (where re-listing is harmless).
/// Pulling the predicate out of the loop body makes the "warn iff Execute"
/// invariant directly unit-testable without driving the sleeping loop.
/// What: returns `true` only for [`DispatchMode::Execute`].
/// Test: `should_warn_only_for_execute`.
pub(crate) fn should_warn(mode: DispatchMode) -> bool {
    matches!(mode, DispatchMode::Execute)
}

/// Drive the `tm watch listen` poll loop until Ctrl-C.
///
/// Why: the long-running entry point. Polling on an interval (rather than a
/// webhook) keeps it dependency-free and locally testable while still achieving
/// "process newly-matched issues each cycle". Trapping SIGINT lets an operator
/// stop it cleanly without orphaning the current cycle.
/// What: loops forever: list label-matched issues via `lister`, [`select_new_issues`]
/// to drop already-seen numbers, [`dispatch_issue`] each new one (honouring the
/// dry-run/execute `mode`), then `tokio::select!` a `sleep(interval)` against
/// `ctrl_c()` so a signal during the sleep breaks out immediately. A failed poll
/// is logged to stderr and retried next cycle rather than aborting the loop.
/// `repo_url`/`base_ref` are the resolved clone URL + default branch threaded
/// into each dispatch.
/// Test: the pure dedup step is unit-tested; the loop wiring (sleep + signal) is
/// exercised manually (it would otherwise block tests).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_listen_loop<L: IssueLister>(
    client: &reqwest::Client,
    url: &str,
    lister: &L,
    settings: &ResolvedWatch,
    repo_url: &str,
    base_ref: &str,
    mode: DispatchMode,
    runtime: RuntimeKind,
) -> anyhow::Result<()> {
    let mut seen: HashSet<u64> = HashSet::new();
    let interval = std::time::Duration::from_secs(settings.interval_secs);
    eprintln!(
        "tm watch listen: polling {} for label `{}` every {}s ({}); Ctrl-C to stop",
        settings.repo,
        settings.label,
        settings.interval_secs,
        match mode {
            DispatchMode::DryRun => "dry-run",
            DispatchMode::Execute => "EXECUTE",
        }
    );
    // The dedup set is process-local (a persistent state file is a documented
    // future enhancement). In execute mode a restart therefore re-dispatches
    // every currently-open matching issue, which is destructive — warn loudly.
    if should_warn(mode) {
        eprintln!(
            "warn: dedup set is in-memory; a restart will re-dispatch all currently-open matching issues"
        );
    }

    loop {
        match lister.list(&settings.repo, &settings.label, settings.state) {
            Ok(issues) => {
                let fresh = select_new_issues(&issues, &mut seen);
                if fresh.is_empty() {
                    eprintln!(
                        "tm watch listen: no new matched issues (seen {} total)",
                        seen.len()
                    );
                } else {
                    for issue in &fresh {
                        if let Err(e) =
                            dispatch_issue(client, url, repo_url, base_ref, issue, mode, runtime)
                                .await
                        {
                            // A dispatch failure for one issue must not kill the
                            // loop or poison the seen-set permanently: drop it
                            // from `seen` so a later cycle can retry it.
                            seen.remove(&issue.number);
                            eprintln!("tm watch listen: dispatch of #{} failed: {e}", issue.number);
                        }
                    }
                }
            }
            Err(e) => {
                eprintln!("tm watch listen: poll failed (will retry): {e}");
            }
        }

        tokio::select! {
            _ = tokio::time::sleep(interval) => {}
            r = tokio::signal::ctrl_c() => {
                match r {
                    Ok(()) => eprintln!("\ntm watch listen: received Ctrl-C, stopping"),
                    Err(e) => eprintln!("\ntm watch listen: signal error ({e}), stopping"),
                }
                break;
            }
        }
    }
    Ok(())
}
