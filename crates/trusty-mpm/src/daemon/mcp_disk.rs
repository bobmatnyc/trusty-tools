//! Daemon-side implementation of the `disk_survey` MCP tool (#6927).
//!
//! Why: `StateBackend` (in `mcp_backend.rs`) has to service the Disk
//! dashboard's survey, and inlining its body there would push that file over
//! the 500-SLOC production cap — the same reason `mcp_console.rs` and
//! `mcp_project.rs` exist. This module is also where the tool's real-world
//! inputs are resolved: the operator's workspace root and keep-list, the live
//! session store, the delegation registry, `gh`, and the daemon's shared size
//! index. [`crate::disk::survey_run`] itself resolves none of them, which is
//! what keeps it hermetically testable.
//! What: one free async function, [`disk_survey`], returning DOC-73 §16.5's
//! tree as JSON.
//! Test: `crate::mcp::tests::dispatch_disk_survey_tool` drives the dispatch
//! path against the mock backend; `crate::disk::survey_tests` covers the survey
//! itself over scratch repositories.
//!
//! READ-ONLY. Nothing here removes, prunes, or writes anything — DOC-73 §16.6
//! item 4 owns the clear action, behind an explicit operator confirm.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::core::trusty_tools_config::{self, TrustyToolsConfig};
use crate::daemon::state::DaemonState;
use crate::disk::size_index::DirSize;
use crate::disk::survey::GroupBy;
use crate::disk::survey_run::{self, DiskProbes, run};
use crate::session_manager::worktree_ownership::AgentWorktreeOwner;
use crate::session_manager::worktree_reclaim::{
    BranchPrState, PrIndex, pr_state_for_branch_within,
};
use crate::session_manager::worktree_reclaim_gh::GH_TIMEOUT;
use crate::session_manager::worktree_registry::ScannedWorktree;
use crate::session_manager::worktree_safety::inspect_dirt;

/// The largest `budget_seconds` this tool will honour (#7313).
///
/// Why 55 and not "whatever the caller asked for": every MCP caller reaches the
/// daemon through `tm serve --stdio`, whose reqwest client carries a 60-second
/// `REQUEST_TIMEOUT` (`crate::bin::tm::commands::serve_stdio`, a path this
/// module cannot import — the two numbers are bound instead by
/// `the_forwarding_timeout_leaves_headroom_over_the_disk_survey_budget_clamp`
/// in that module's own tests). A larger budget therefore cannot produce a
/// survey the caller will ever see: the bridge gives up at 60 s and returns a
/// transport error while the daemon keeps working, which is the #6929 failure
/// mode over again — a 502 carrying no survey where a truncated one was
/// available. 55 leaves five seconds for serialization and the round trip.
/// The console already clamps on its own side (`trusty-console`'s
/// `routes/disk.rs`); this is the same rule at the tool, for every other caller.
/// Test: `a_budget_past_the_bridge_timeout_is_clamped`.
pub const MAX_BUDGET_SECONDS: u64 = 55;

/// Apply [`MAX_BUDGET_SECONDS`], saying whether it bit (#7313).
///
/// What: the budget to use, and whether the caller's figure was reduced — which
/// the response reports as `budget_clamped`, so a caller that asked for 120 and
/// got a partial pass can tell the clamp from a genuinely slow fleet.
/// Test: `a_budget_past_the_bridge_timeout_is_clamped`,
/// `a_budget_inside_the_bridge_timeout_is_untouched`.
fn clamp_budget(requested: Option<u64>) -> (Option<u64>, bool) {
    match requested {
        Some(s) if s > MAX_BUDGET_SECONDS => (Some(MAX_BUDGET_SECONDS), true),
        other => (other, false),
    }
}

/// Read the caller's `group_by` argument (#7313).
///
/// Why an error rather than a silent fall-through on an unknown value: a typo
/// would otherwise return a complete, plausible survey with the roll-up the
/// caller asked for simply absent, and nothing in the payload saying why.
/// Test: `an_unknown_group_by_is_rejected`.
fn parse_group_by(group_by: Option<&str>) -> Result<GroupBy, String> {
    match group_by {
        None => Ok(GroupBy::None),
        Some("session") => Ok(GroupBy::Session),
        Some(other) => Err(format!(
            "disk_survey: unknown `group_by` value `{other}`; the only supported value is `session`"
        )),
    }
}

/// Survey every managed worktree for the console Disk view (#6927).
///
/// Why: see the module docs. The whole pass runs on the blocking pool because
/// every probe under it is a synchronous subprocess — `git status`,
/// `git worktree list`, `gh pr list` — and a byte walk; running that on a
/// runtime worker would stall the daemon's other routes.
/// What: resolves the workspace root and keep-list from the operator's config,
/// snapshots the live session claims, and hands `disk::survey_run::run` a
/// `gh`-backed pull-request probe plus a measurement closure over the daemon's
/// SHARED [`crate::disk::size_index::DirSizeIndex`] — never a second index, so
/// a poll every few seconds costs cache hits rather than walks, and the mutex
/// is held for each measurement alone rather than for the whole pass.
/// `budget_seconds` bounds the whole pass, byte walks included (#6929);
/// worktrees past it are listed as `review`, never omitted and never `stale`,
/// and a project or root past it reports no byte figure. It is clamped to
/// [`MAX_BUDGET_SECONDS`] (#7313) — see that constant for why a larger figure
/// cannot produce a survey any caller receives.
/// `group_by` (#7313) adds the per-session roll-up to the response.
/// Test: `crate::disk::survey_tests`, and `dispatch_disk_survey_tool` for the
/// dispatch wiring.
pub async fn disk_survey(
    state: &Arc<DaemonState>,
    project: Option<&str>,
    budget_seconds: Option<u64>,
    group_by: Option<&str>,
) -> Result<Value, String> {
    let group_by = parse_group_by(group_by)?;
    // #7313: clamped BEFORE the deadline is computed, so the clamp is what the
    // pass actually runs under rather than a figure reported beside a longer one.
    let (budget_seconds, budget_clamped) = clamp_budget(budget_seconds);
    let config = TrustyToolsConfig::load();
    let repos_root = trusty_tools_config::workspace_root(&config);
    // #6927: read FALLIBLY, separately from the lenient `load` above. A config
    // that will not parse yields a keep-list that keeps everything and says so
    // in `keep_list.error`, rather than an empty one that protects nothing.
    let keep_list = trusty_tools_config::load_disk_keep_list();
    // The claim set is a SNAPSHOT, which is correct here and would not be on a
    // destructive path: this tool only displays, and the delete path re-reads
    // liveness per candidate immediately before each removal (#2919).
    let manager = state.session_manager().await;
    // #7232: through the crate's single claim producer, so this report and the
    // reclaim sweep agree about which claims are tombstones. The console is not
    // a managed session, so it names no caller — every LIVE claim it sees is
    // foreign, which is the conservative reading.
    let claims = manager.workspace_claims(None).await;
    let index = state.disk_size_index();
    let state_for_agents = Arc::clone(state);
    let project = project.map(str::to_string);
    let deadline = budget_seconds.map(|s| Instant::now() + Duration::from_secs(s));
    // #7357: this tool is the entry point, so it resolves the adopted anchors
    // under the daemon's own framework root; `survey_run::run` takes them.
    let adopted = crate::project::adopted_anchors_under(state.framework_root());

    tokio::task::spawn_blocking(move || {
        let agent_state = |owner: &AgentWorktreeOwner| {
            crate::daemon::services::agent_worktree_reap::delegation_state_for_agent(
                &state_for_agents,
                &owner.agent_id,
            )
        };
        // One `gh` index per repository, not one per worktree — the same
        // amortization `survey_with_index` applies, for the same reason.
        let indexes: RefCell<BTreeMap<PathBuf, PrIndex>> = RefCell::new(BTreeMap::new());
        // #6929: the survey's remaining time is the ceiling on both `gh` calls
        // this probe can make, so a worktree admitted late cannot spend twenty
        // seconds of `GH_TIMEOUT` past the deadline the console is holding.
        let pr_state = |scanned: &ScannedWorktree, left: Option<Duration>| -> BranchPrState {
            pr_for(&indexes, scanned, left)
        };
        // #6927 review: the lock spans ONE measurement, not the pass. Held
        // across `run` it would also cover every `git status` and `gh pr list`
        // that `classify` shells out to, so a second `disk_survey` — or the
        // #6926 background refresher — would queue behind minutes of network
        // work for an index it only wanted to read.
        // #6929: `budget` is whatever the survey deadline has left. Passing it
        // through is what stops one cold walk from spending the index's fixed
        // 30-second ceiling and overrunning the survey the console is waiting
        // on.
        let measure = |path: &Path, budget: Option<Duration>| -> Option<DirSize> {
            let mut index = index.lock();
            survey_run::measure(&mut index, path, budget)
        };
        let probes = DiskProbes {
            pr_state: &pr_state,
            claims: &claims,
            agent_state: &agent_state,
            dirt: &inspect_dirt,
            measure: &measure,
        };
        let survey = run(
            &repos_root,
            &keep_list,
            &probes,
            deadline,
            project.as_deref(),
            group_by,
            &adopted,
        );
        let mut value = serde_json::to_value(survey)
            .map_err(|e| format!("disk_survey: serialize error: {e}"))?;
        // #7313: the clamp is a TRANSPORT fact, not a survey fact — the survey
        // ran a full pass under whatever deadline it was handed and has nothing
        // to say about what the caller originally asked for. Recorded here, on
        // the response, so it stays out of `DiskSurvey`'s shape.
        if let Value::Object(map) = &mut value {
            map.insert("budget_clamped".to_string(), Value::Bool(budget_clamped));
        }
        Ok(value)
    })
    .await
    .map_err(|e| format!("disk_survey: the survey pass panicked: {e}"))?
}

/// The pull-request state of one scanned worktree's branch.
///
/// Why: named rather than inlined so the truncation fallback is stated once.
/// The bulk index cannot reach a branch older than its window, and on this
/// repository that is nearly every worktree the dashboard is about — so an
/// unresolved branch is retried with a targeted lookup, exactly as the reclaim
/// survey does. Without it the view would render `review` for worktrees whose
/// pull requests merged months ago.
///
/// What `left` does (#6929): it is the survey's remaining wall clock, and it
/// bounds BOTH calls rather than each of them separately. `GH_TIMEOUT` is a
/// fixed ten seconds, and this function can spend it twice, so a worktree the
/// loop admitted with a millisecond left used to run twenty seconds past the
/// survey's deadline — past the console's thirty-second transport too, which
/// is why the Disk view answered 502 with no survey rather than a truncated
/// one. The fallback is skipped outright when the first call consumed the
/// budget: the bulk index's own answer (`Unknown` or `LookupFailed`) stands,
/// and both block, so skipping it can only be conservative.
/// Test: `a_budgeted_survey_answers_within_its_budget` covers the contract in
/// `survey_run`; this wiring is exercised by `dispatch_disk_survey_tool`.
fn pr_for(
    indexes: &RefCell<BTreeMap<PathBuf, PrIndex>>,
    scanned: &ScannedWorktree,
    left: Option<Duration>,
) -> BranchPrState {
    // A local deadline, so the SECOND call is bounded by what the first one
    // left rather than by the same figure over again.
    let until = left.map(|l| Instant::now() + l);
    let mut cache = indexes.borrow_mut();
    let index = cache
        .entry(scanned.registry_root.clone())
        .or_insert_with(|| PrIndex::from_gh_within(&scanned.registry_root, gh_budget(until)));
    let pr = index.state_for(scanned.branch.as_deref());
    if matches!(
        pr,
        BranchPrState::Unknown | BranchPrState::LookupFailed { .. }
    ) && !index.is_complete()
        && let Some(branch) = scanned.branch.as_deref()
    {
        let budget = gh_budget(until);
        if !budget.is_zero() {
            return pr_state_for_branch_within(&scanned.registry_root, branch, budget);
        }
    }
    pr
}

/// How long a `gh` call may run, given the survey's own deadline.
///
/// What: the time left, capped at [`GH_TIMEOUT`]; [`GH_TIMEOUT`] when the
/// survey has no deadline; and `Duration::ZERO` when it has none left, which
/// the caller reads as "do not start this call".
/// Test: `gh_budget_never_exceeds_the_fixed_ceiling`,
/// `gh_budget_is_zero_once_the_survey_deadline_has_passed`.
fn gh_budget(until: Option<Instant>) -> Duration {
    match until {
        None => GH_TIMEOUT,
        Some(d) => d
            .checked_duration_since(Instant::now())
            .unwrap_or(Duration::ZERO)
            .min(GH_TIMEOUT),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gh_budget_never_exceeds_the_fixed_ceiling() {
        let generous = Instant::now() + GH_TIMEOUT * 10;
        assert_eq!(gh_budget(Some(generous)), GH_TIMEOUT);
        assert_eq!(gh_budget(None), GH_TIMEOUT);
    }

    #[test]
    fn gh_budget_is_zero_once_the_survey_deadline_has_passed() {
        let spent = Instant::now() - Duration::from_secs(1);
        assert_eq!(gh_budget(Some(spent)), Duration::ZERO);
    }

    /// A budget past the stdio bridge's timeout is cut down to the clamp.
    ///
    /// Why this is a bug and not a preference: the daemon honoured a
    /// `budget_seconds: 120` faithfully and kept surveying, while the bridge
    /// carrying the answer gave up at 60 seconds and returned a transport
    /// error. The caller saw no survey at all where a 55-second truncated one
    /// was available — the #6929 failure mode, reached through the argument
    /// rather than through a slow fleet.
    ///
    /// Fails before the change: `clamp_budget` did not exist and 120 was passed
    /// through to the deadline verbatim.
    #[test]
    fn a_budget_past_the_bridge_timeout_is_clamped() {
        assert_eq!(clamp_budget(Some(120)), (Some(MAX_BUDGET_SECONDS), true));
        assert_eq!(
            Duration::from_secs(MAX_BUDGET_SECONDS),
            Duration::from_secs(55),
            "the clamp the schema's `maximum` advertises"
        );
    }

    #[test]
    fn a_budget_inside_the_bridge_timeout_is_untouched() {
        assert_eq!(clamp_budget(Some(30)), (Some(30), false));
        assert_eq!(
            clamp_budget(Some(MAX_BUDGET_SECONDS)),
            (Some(MAX_BUDGET_SECONDS), false),
            "the ceiling itself is not a clamp"
        );
        assert_eq!(clamp_budget(None), (None, false));
    }

    #[test]
    fn group_by_session_is_the_one_accepted_value() {
        assert_eq!(parse_group_by(None), Ok(GroupBy::None));
        assert_eq!(parse_group_by(Some("session")), Ok(GroupBy::Session));
    }

    /// An unknown `group_by` is an error, never a silent full survey.
    ///
    /// Why: a typo would otherwise return a complete, plausible payload with
    /// the roll-up simply missing and nothing saying why.
    #[test]
    fn an_unknown_group_by_is_rejected() {
        let err = parse_group_by(Some("project")).expect_err("unknown values must not be accepted");
        assert!(err.contains("project"), "{err}");
        assert!(
            err.contains("session"),
            "the error must name what IS accepted: {err}"
        );
    }

    #[test]
    fn gh_budget_hands_back_the_time_that_is_actually_left() {
        let tight = Instant::now() + Duration::from_millis(200);
        let budget = gh_budget(Some(tight));
        assert!(!budget.is_zero(), "{budget:?}");
        assert!(budget <= Duration::from_millis(200), "{budget:?}");
    }
}
