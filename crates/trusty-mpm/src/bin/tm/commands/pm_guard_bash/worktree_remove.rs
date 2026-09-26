//! `tm hook --pm-guard` — agent-side `git worktree remove`, denied to every
//! agent but one (#5791, narrowed by
//! [ADR-0057](../../../../../../../docs/adr/0057-version-control-owns-worktree-removal.md)).
//!
//! Why: removing a merged worktree could not be delegated at all. An
//! unisolated dispatch is denied by the #4480 shared-HEAD guard, and an
//! isolated agent's git operations are confined to its own worktree, so it
//! cannot act on the shared registry either. The owner ruled on 2026-08-19
//! that this is not a hole to open: worktree removal is PM-executed via a `tm`
//! command. Prose alone does not carry that. `BASE-AGENT.md` told every agent
//! for months to "remove your worktree" after a merge, and an agent that reads
//! a stale copy of that instruction still reaches for `git worktree remove`.
//!
//! On 2026-09-02 the owner re-ruled the cleanup half for ONE role:
//! "[worktree removal] should be handled by version-manager which should not
//! be just versions and branches but worktrees as well." ADR-0057 records that
//! grant and scopes it — `version-control` only, `remove` only, and only on a
//! tree the guard can prove is safe to delete.
//!
//! What: [`evaluate_worktree_remove_command`] classifies a `git worktree
//! remove` segment into [`WorktreeRemoveVerdict`]. Every subagent but
//! `version-control` is denied with the unchanged #5791 text. `version-control`
//! is denied too unless the payload proves a genuine subagent dispatch and the
//! target is a harness worktree, and otherwise yields
//! [`WorktreeRemoveVerdict::ReCheck`] — an allowance the caller must still earn
//! by running [`super::worktree_remove_rechecks::evaluate_removal_rechecks`].
//! `list`, `prune`, `lock`, and `move` are untouched — this rule is about
//! destroying a checkout, not about reading or repairing the registry. The PM
//! is never denied.
//!
//! **`agent_type` alone is never the grant.** A top-level session launched with
//! `--agent version-control` also carries that field
//! (`pm_guard::payload_is_subagent_dispatch` documents why), so the grant reads
//! it only alongside a non-empty `agent_id`, which the hooks contract stamps
//! only inside a subagent call. A payload that claims the name without the id
//! is refused, and the refusal says so.
//!
//! Caller context comes from [`super::super::pm_guard_fanout::caller_is_subagent`],
//! which FAILS OPEN — an indeterminate context reads as "not a subagent" and
//! allows. That asymmetry is deliberate there and inherited here: a false DENY
//! would land on the PM, the one session that must keep working. The ADR-0057
//! grant does NOT inherit it: every one of its re-checks fails closed.
//!
//! Test: `denies_worktree_remove_from_a_subagent`,
//! `allows_worktree_remove_from_the_pm`,
//! `allows_non_remove_worktree_subcommands`,
//! `denies_a_remove_hidden_in_a_composed_command`,
//! `allows_worktree_remove_from_version_control_on_clean_merged_unowned_tree`,
//! `denies_worktree_remove_from_version_control_when_tree_dirty`,
//! `denies_worktree_remove_from_version_control_when_commits_are_unpushed`,
//! `denies_worktree_remove_from_version_control_when_no_merged_pr`,
//! `denies_worktree_remove_from_version_control_when_another_agent_holds_lock`,
//! `denies_worktree_remove_from_version_control_when_the_owner_query_fails`,
//! `denies_worktree_remove_when_agent_type_claims_version_control_without_agent_id`,
//! `denies_a_removal_whose_path_carries_an_unexpanded_variable`,
//! `denies_a_removal_whose_dash_c_carries_an_unexpanded_variable`,
//! `a_clean_tree_whose_commits_are_all_on_origin_needs_no_pull_request` (#7914),
//! `a_commit_no_origin_ref_has_still_denies_without_a_merged_pr`,
//! `a_detached_head_that_is_a_merged_prs_own_head_is_reclaimable` (#7832),
//! `a_detached_head_no_merged_pr_carries_still_denies`,
//! `a_detached_head_matched_to_a_pull_request_with_another_head_denies`,
//! `a_detached_head_matched_to_a_pull_request_with_no_head_denies`,
//! `an_unanswerable_commit_search_denies_a_detached_head`,
//! `an_unresolvable_head_sha_denies_a_detached_head`,
//! `a_head_sha_matching_the_merged_prs_own_head_grants_despite_a_stale_upstream` (#7958),
//! `a_head_that_is_not_the_merged_prs_head_still_denies_when_ahead`,
//! `an_unanswerable_head_sha_never_grants_an_ahead_worktree`,
//! `a_merged_pr_carrying_no_head_sha_never_grants_an_ahead_worktree`,
//! `version_control_reaches_the_rechecks_for_a_sibling_layout_worktree` (#8413)
//! below; `pm_guard_denies_worktree_remove_from_native_subagent` and
//! `pm_guard_allows_worktree_remove_from_pm` run the binary end to end in
//! `tests/tm_hook_pm_guard.rs`.

use std::path::{Path, PathBuf};

use trusty_mpm::core::dispatch_isolation::permitted_in_shared_checkout;
use trusty_mpm::core::project_aliases::{is_sibling_worktree_path, is_worktree_path};

use super::main_checkout::git_verb_target_dir_with_tail;
use super::worktree_remove_rechecks::{
    CHECK_DISPATCH_IDENTITY, CHECK_WORKTREE_SCOPE, recheck_deny,
};
use super::{PathEnv, resolve_target_path, unresolved_target};

/// Deny reason for an agent-side `git worktree remove` (#5791, ADR-0057).
///
/// Why: a bare refusal makes the model retry or hand-roll a `rm -rf`, which is
/// the worse outcome — it destroys unsaved work and leaves a stale registry
/// entry behind, so the text forecloses it explicitly rather than leaving it
/// as the obvious next thing to try. The text also names the ruling and what
/// the agent should do instead — hand the tree back and stop. It says which
/// worktree verbs still work, so an agent reading a registry does not treat the
/// whole subcommand as blocked. Since ADR-0057 it also names the one role the
/// deny no longer reaches, so an agent that has seen `version-control` do this
/// does not read its own deny as a bug.
/// What: the `permissionDecisionReason` string emitted on this deny.
/// Test: `denies_worktree_remove_from_a_subagent`,
/// `the_agent_side_worktree_denies_hand_back_and_never_name_a_force_sweep`.
// #8577: the remedy named a fleet-wide `--force` sweep; it is now the same
// single-tree hand-back `recheck_deny` gives.
pub(crate) const WORKTREE_REMOVE_DENY_REASON: &str = "Worktree removal is PM-executed (#5791, owner ruling 2026-08-19): an agent never removes a \
     worktree, its own included. Instead, hand it back: report the worktree path and its merged \
     PR to the PM (or to `version-control`), then stop. A sweep over other worktrees is never \
     the fallback for one refused removal. `rm -rf` on the worktree directory is not the \
     workaround either — it destroys unsaved work and leaves a stale registry entry git still \
     believes in. `git worktree list` and `git worktree prune` are not blocked, and SendMessage \
     is never blocked — use it to report the path back. One role is exempt and it is not this \
     one: ADR-0057 lets a dispatched `version-control` agent remove a tree the guard can prove \
     is a harness worktree, clean, merged on GitHub, and held by nobody else.";

/// The two payload fields the ADR-0057 grant reads, unresolved.
///
/// Why: the grant must AND them, and doing that here rather than at the call
/// site is what keeps `agent_type` from being trusted on its own — a caller
/// cannot hand this rule a pre-resolved "yes, it's version-control".
/// What: `agent_id` is present only inside a genuine subagent dispatch;
/// `agent_type` is also stamped on a top-level `--agent` session.
/// Test: `denies_worktree_remove_when_agent_type_claims_version_control_without_agent_id`.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct DispatchIdentity<'a> {
    /// `payload.agent_id`.
    pub(crate) agent_id: Option<&'a str>,
    /// `payload.agent_type`.
    pub(crate) agent_type: Option<&'a str>,
}

impl<'a> DispatchIdentity<'a> {
    /// Read both fields off a `PreToolUse` payload.
    pub(crate) fn from_payload(payload: &'a serde_json::Value) -> Self {
        let field = |key: &str| {
            payload
                .get(key)
                .and_then(serde_json::Value::as_str)
                .filter(|s| !s.is_empty())
        };
        Self {
            agent_id: field("agent_id"),
            agent_type: field("agent_type"),
        }
    }

    /// Is this a genuine `version-control` subagent dispatch (ADR-0057)?
    ///
    /// The name comes from `dispatch_isolation`'s
    /// `SHARED_CHECKOUT_PERMITTED_NAMES` through
    /// [`permitted_in_shared_checkout`], so the dispatch-time grant and this
    /// Bash-time one can never read different lists.
    fn is_permitted_remover(self) -> bool {
        self.agent_id.is_some() && self.agent_type.is_some_and(permitted_in_shared_checkout)
    }

    /// Does the payload CLAIM the permitted name without proving a dispatch?
    fn claims_the_name_without_a_dispatch(self) -> bool {
        self.agent_id.is_none() && self.agent_type.is_some_and(permitted_in_shared_checkout)
    }
}

/// What the guard has decided about a `git worktree remove` (ADR-0057).
///
/// Why: the rule used to be a boolean, and ADR-0057 adds a third answer — a
/// removal that is permitted in principle but not yet earned. Making that its
/// own variant is what stops the caller from allowing a `version-control`
/// removal it has not re-checked.
/// What: `ReCheck` carries the absolute directory the command would delete.
/// Test: as the module doc.
pub(crate) enum WorktreeRemoveVerdict {
    /// Nothing to decide — not a removal, or not a caller this rule binds.
    Allow,
    /// Refuse, with the reason already built.
    Deny(String),
    /// A `version-control` dispatch aimed at a harness worktree. Allowed only
    /// after `worktree_remove_rechecks::evaluate_removal_rechecks` passes.
    ReCheck {
        /// The directory the removal would delete.
        target: PathBuf,
    },
}

/// Classify a Bash command for agent-side worktree removal.
///
/// Why: kept free of environment reads apart from the shared [`PathEnv`], so
/// the policy is exhaustively unit testable and the caller context stays
/// resolved in `pm_guard_fanout`.
/// What: allows outright when the caller is not a subagent or no composition
/// segment is a `git worktree remove`. Otherwise the ADR-0057 identity test
/// decides: a genuine `version-control` dispatch aimed at a path under a
/// harness worktree root yields [`WorktreeRemoveVerdict::ReCheck`], and every
/// other caller — including a payload that claims the name without an
/// `agent_id` — is denied.
/// Test: the nine cases named in the module docs.
pub(crate) fn evaluate_worktree_remove_command(
    command: &str,
    caller_is_subagent: bool,
    identity: DispatchIdentity<'_>,
    cwd: &Path,
) -> WorktreeRemoveVerdict {
    if !caller_is_subagent {
        return WorktreeRemoveVerdict::Allow;
    }
    let Some((target_dir, tail)) = worktree_remove_segment(command, cwd) else {
        return WorktreeRemoveVerdict::Allow;
    };
    if identity.claims_the_name_without_a_dispatch() {
        return WorktreeRemoveVerdict::Deny(recheck_deny(
            CHECK_DISPATCH_IDENTITY,
            &target_dir,
            "the payload names `version-control` but carries no `agent_id`, which the hooks \
             contract stamps only inside a subagent call — a top-level session launched with \
             `--agent version-control` carries the same `agent_type` and inherits nothing from \
             this grant.",
        ));
    }
    if !identity.is_permitted_remover() {
        return WorktreeRemoveVerdict::Deny(WORKTREE_REMOVE_DENY_REASON.to_string());
    }
    // Re-check `e`, run here because it is lexical: an out-of-scope target
    // must not cost a daemon round trip or a `gh` call to refuse.
    let Some((token, target)) = removal_target_path(&tail, &target_dir) else {
        return WorktreeRemoveVerdict::Deny(recheck_deny(
            CHECK_WORKTREE_SCOPE,
            &target_dir,
            "the command names no removable path this guard can resolve, so it cannot establish \
             what would be deleted.",
        ));
    };
    // #7098: an unexpanded `$MAIN` survives `resolve_target_path` as a literal
    // path component and is joined once per token, so `git -C $MAIN worktree
    // remove $MAIN/…` probed `<repo>/$MAIN/$MAIN/…` and the failure surfaced as
    // a `clean-tree` deny quoting a directory the command never named. Refuse
    // here instead, against the token as written.
    // #7234: a leading `~` survives the same way when `$HOME` is unset.
    if let Some(unresolved) = unresolved_target(&target) {
        let expansion = unresolved.token;
        return WorktreeRemoveVerdict::Deny(recheck_deny(
            CHECK_WORKTREE_SCOPE,
            Path::new(&token),
            &format!(
                "the path still carries the unresolved shell expansion `{expansion}` — the guard \
                 expands `$TMPDIR`, `$TMP`, `$HOME` and `$PWD`, and a leading `~` only when \
                 `$HOME` is set, so it cannot establish which directory would be deleted, and \
                 every later re-check would probe a path that does not exist. Re-run the removal \
                 with the worktree path written out in full."
            ),
        ));
    }
    // #8413: the `<repo>-worktrees/<tree>` sibling layout is in scope too, but
    // only for a LINKED worktree (`.git` is a file) — a main checkout that
    // merely sits under a `*-worktrees` directory stays out of reach.
    let sibling = is_sibling_worktree_path(&target) && target.join(".git").is_file();
    if !is_worktree_path(&target) && !sibling {
        return WorktreeRemoveVerdict::Deny(recheck_deny(
            CHECK_WORKTREE_SCOPE,
            &target,
            "the target is not under a harness worktree root (`.claude/worktrees/`, \
             `.worktrees/`, or a linked worktree in a `<repo>-worktrees/` sibling), and the \
             grant reaches no other directory.",
        ));
    }
    WorktreeRemoveVerdict::ReCheck { target }
}

/// The first composition segment that is a `git worktree remove`.
///
/// Why: a forbidden verb hides in any segment, not just the first
/// (`cargo test && git worktree remove …`), and `git -C <path>` moves the
/// directory the removal resolves against. Both are already handled by
/// [`git_verb_target_dir_with_tail`], which is why this reuses it rather than
/// re-lexing. Residual bypasses are that walker's and are unchanged: a verb
/// built by variable expansion or hidden in a command substitution is not
/// resolved, and — largest of them, tracked as
/// [issue #3981](https://github.com/bobmatnyc/trusty-tools/issues/3981) —
/// `pm_guard`'s Guard 2/3 escape hatches
/// (`TRUSTY_MPM_DISABLE_HOOKS`/`TRUSTY_MPM_PM_UNRESTRICTED`) bypass this rule
/// entirely when set.
/// What: `Some((effective directory, argv tail from `worktree` onward))`.
/// Test: `denies_a_remove_hidden_in_a_composed_command`.
fn worktree_remove_segment(command: &str, cwd: &Path) -> Option<(PathBuf, Vec<String>)> {
    git_verb_target_dir_with_tail(command, cwd, &PathEnv::from_process(), |verb, tail| {
        verb == "worktree" && tail.first().map(String::as_str) == Some("remove")
    })
    .map(|(_, dir, tail)| (dir, tail))
}

/// The absolute path a `git worktree remove` tail names.
///
/// Why: the re-checks all key on the directory that would be deleted, and it
/// is the one thing the command says that the guard cannot infer.
/// What: the first tail token after `remove` that is not a flag, returned both
/// AS WRITTEN and resolved against `base` through the shared
/// [`resolve_target_path`]. `git worktree remove` takes no option that consumes
/// a value, so skipping every `-`-led token cannot swallow the path. The raw
/// token is kept because a refusal about an unresolvable path must quote what
/// the command said, not the path the guard synthesized from it (#7098).
/// Test: `resolves_the_removal_target_against_a_dash_c_directory`,
/// `denies_a_removal_whose_path_carries_an_unexpanded_variable`.
fn removal_target_path(tail: &[String], base: &Path) -> Option<(String, PathBuf)> {
    let arg = tail
        .iter()
        .skip(1)
        .find(|t| !t.starts_with('-') && !t.is_empty())?;
    Some((
        arg.clone(),
        resolve_target_path(arg, base, &PathEnv::from_process()),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::pm_guard_bash::worktree_remove_rechecks::{
        CHECK_CLEAN_TREE, CHECK_LOCAL_ONLY_COMMITS, CHECK_MERGED_PULL_REQUEST, CHECK_SOLE_OWNER,
        CHECK_UNPUSHED_COMMITS, evaluate_removal_rechecks,
    };
    // #7889: the admission's slug is spelled once, in the shared predicate.
    use trusty_mpm::core::worktree_carried_by_pr::{
        CarriedByPr, MERGED_PR_ANCESTRY_CHECK as CHECK_MERGED_PR_ANCESTRY,
    };
    use trusty_mpm::core::worktree_landed_content::{
        LANDED_CONTENT_CHECK as CHECK_LANDED_CONTENT, LandedContent, LandingAdmission,
    };
    use trusty_mpm::core::worktree_landed_history::ContentOnBase;
    use trusty_mpm::core::worktree_removal_facts::{
        MergedPrLookup, UpstreamComparison, WorktreeRemovalProbe,
    };

    /// The subagent shape the #5791 deny still binds in full.
    fn engineer() -> DispatchIdentity<'static> {
        DispatchIdentity {
            agent_id: Some("agent-abc123"),
            agent_type: Some("rust-engineer"),
        }
    }

    /// A genuine `version-control` dispatch: both fields present.
    fn version_control() -> DispatchIdentity<'static> {
        DispatchIdentity {
            agent_id: Some("agent-abc123"),
            agent_type: Some("version-control"),
        }
    }

    fn deny_reason(v: WorktreeRemoveVerdict) -> String {
        match v {
            WorktreeRemoveVerdict::Deny(r) => r,
            WorktreeRemoveVerdict::Allow => panic!("expected a deny, got Allow"),
            WorktreeRemoveVerdict::ReCheck { target } => {
                panic!("expected a deny, got ReCheck({})", target.display())
            }
        }
    }

    fn recheck_target(v: WorktreeRemoveVerdict) -> PathBuf {
        match v {
            WorktreeRemoveVerdict::ReCheck { target } => target,
            WorktreeRemoveVerdict::Allow => panic!("expected ReCheck, got Allow"),
            WorktreeRemoveVerdict::Deny(r) => panic!("expected ReCheck, got Deny: {r}"),
        }
    }

    fn is_allow(v: &WorktreeRemoveVerdict) -> bool {
        matches!(v, WorktreeRemoveVerdict::Allow)
    }

    /// A worktree path the lexical scope check accepts.
    const WT: &str = "/repo/.claude/worktrees/agent-x";

    /// Fabricated answers, so no test reaches git, GitHub or the daemon.
    struct FakeProbe {
        dirty: Result<usize, String>,
        unpushed: Result<UpstreamComparison, String>,
        branch: Result<String, String>,
        /// The answer for the CHECKED-OUT branch name.
        merged: Result<MergedPrLookup, String>,
        /// #7275 round 2: the answer for the round-stripped stem, which the
        /// guard asks for only when the branch itself has no merged pull
        /// request. `None` means GitHub reported nothing for it either.
        merged_related: Option<Result<MergedPrLookup, String>>,
        /// #7275, #8633: what `content_on_base` answers for this tree's base.
        on_base: Result<ContentOnBase, String>,
        /// #7914: commits reachable from HEAD that no `origin` ref has. One by
        /// default, so every pre-#7914 test still reaches the merged-PR route
        /// it was written for — the admission only ever fires on a zero.
        local_only: Result<usize, String>,
        /// #7832, #7958: the object id this worktree's HEAD resolves to.
        head_sha: Result<String, String>,
        /// #7832: the MERGED pull request opened from that exact commit, if
        /// any. Reached only on the detached-HEAD route.
        commit_pr: Result<MergedPrLookup, String>,
        /// #7275 round 2: the base ref the merge-tree question was asked
        /// against, so a test can prove it came from the pull request.
        asked_base: std::cell::RefCell<Option<String>>,
        /// #7889: the landed-content admission's answer. Undeterminable by
        /// default, so every pre-#7889 expectation stands and a test that
        /// wants the admission has to say so.
        landed: LandedContent,
        /// #7889 route (c): whether a merged pull request carried HEAD.
        carried: Option<CarriedByPr>,
        /// #7889: `Some(true)` when the admission reused the #7914 fetch,
        /// `Some(false)` when it fetched again, `None` when never asked.
        asked_fresh: std::cell::Cell<Option<bool>>,
        /// #7889 critic round 2: the nested-repository scan's answer.
        nested: Result<Option<String>, String>,
    }

    /// A merged-PR answer for the fixture repository (#7057), landing on `main`.
    fn lookup(count: usize) -> MergedPrLookup {
        lookup_on(count, if count == 0 { "" } else { "main" })
    }

    /// A merged-PR answer that landed on a named base (#7275 round 2), carrying
    /// the pull request's own head commit (#7958).
    fn lookup_on(count: usize, base: &str) -> MergedPrLookup {
        MergedPrLookup::new(count, FAKE_REPO, base).with_head_sha(if count == 0 {
            ""
        } else {
            MERGED_PR_HEAD
        })
    }

    /// The answer the #7832 commit search gives: `count` merged pull requests
    /// opened from exactly this worktree's HEAD.
    fn commit_lookup(count: usize) -> MergedPrLookup {
        MergedPrLookup::new(count, FAKE_REPO, "").with_head_sha(if count == 0 {
            ""
        } else {
            WORKTREE_HEAD
        })
    }

    /// The commit a fixture worktree's HEAD sits on (#7832, #7958).
    const WORKTREE_HEAD: &str = "02a83032d1f0b4c9e7a6d5c4b3a291807f6e5d4c";

    /// The commit the merged pull request's head branch pointed at — a
    /// DIFFERENT object id from [`WORKTREE_HEAD`], so a test that wants the
    /// #7958 grant has to say so.
    const MERGED_PR_HEAD: &str = "9c8699fe0a1b2c3d4e5f60718293a4b5c6d7e8f9";

    /// The repository the fake probe reports having searched (#7057).
    const FAKE_REPO: &str = "1m-consulting/adaptive-crm";

    /// #8633: HEAD is an ancestor of the base.
    fn tip_landed() -> ContentOnBase {
        ContentOnBase::Landed { at: None }
    }

    /// #8633: a clean merge into the tip that still changes `src/lib.rs`,
    /// after an exhaustive history walk found no landing commit.
    fn residual() -> ContentOnBase {
        ContentOnBase::Residual {
            paths: vec!["src/lib.rs".into()],
            searched: 0,
            candidates: 0,
        }
    }

    impl FakeProbe {
        /// Clean, pushed, on a branch with one merged pull request.
        fn reclaimable() -> Self {
            Self {
                dirty: Ok(0),
                unpushed: Ok(UpstreamComparison::Ahead(0)),
                branch: Ok("feat/thing".to_string()),
                merged: Ok(lookup(1)),
                merged_related: None,
                // #7275: a tree with its own merged PR and a live, level
                // upstream is never asked this; a false default keeps the
                // merged-PR arm the only thing granting here.
                on_base: Ok(residual()),
                // #7914: not zero — a fixture that admitted here would stop
                // exercising the merged-PR route these tests exist for.
                local_only: Ok(1),
                // #7958: NOT the merged pull request's head by default, so the
                // head-sha grant fires only where a test asks for it and every
                // pre-#7958 expectation stands.
                head_sha: Ok(WORKTREE_HEAD.to_string()),
                // #7832: no pull request was opened from this commit, for the
                // same reason.
                commit_pr: Ok(commit_lookup(0)),
                asked_base: std::cell::RefCell::new(None),
                // #7889: the admission establishes nothing unless a test asks
                // it to, so no pre-#7889 fixture can grant through it.
                landed: LandedContent::unavailable("no landed-content answer was fabricated"),
                // #7889 route (c): not asked unless a test states it.
                carried: None,
                asked_fresh: std::cell::Cell::new(None),
                nested: Ok(None),
            }
        }

        /// #7275: the round-N sibling shape — no pull request carries this
        /// branch's own name, its round-1 sibling's DID merge, and its content
        /// is already on that pull request's base.
        fn round_sibling() -> Self {
            Self {
                branch: Ok("feat/thing-r2".to_string()),
                merged: Ok(lookup(0)),
                merged_related: Some(Ok(lookup(1))),
                on_base: Ok(tip_landed()),
                ..Self::upstream_deleted()
            }
        }

        /// The shape `gh pr merge --delete-branch` leaves behind (#7232): the
        /// remote branch is gone, so `@{upstream}` no longer resolves.
        fn upstream_deleted() -> Self {
            Self {
                unpushed: Ok(UpstreamComparison::NoUpstream),
                ..Self::reclaimable()
            }
        }
    }

    impl WorktreeRemovalProbe for FakeProbe {
        fn dirty_entries(&self, _dir: &Path) -> Result<usize, String> {
            self.dirty.clone()
        }
        fn unpushed_commits(&self, _dir: &Path) -> Result<UpstreamComparison, String> {
            self.unpushed.clone()
        }
        fn branch(&self, _dir: &Path) -> Result<String, String> {
            self.branch.clone()
        }
        fn local_only_commits(&self, _dir: &Path) -> Result<usize, String> {
            self.local_only.clone()
        }
        fn head_sha(&self, _dir: &Path) -> Result<String, String> {
            self.head_sha.clone()
        }
        fn merged_pull_request_for_commit(
            &self,
            _dir: &Path,
            _sha: &str,
        ) -> Result<MergedPrLookup, String> {
            self.commit_pr.clone()
        }
        fn merged_pull_requests(
            &self,
            _dir: &Path,
            branch: &str,
        ) -> Result<MergedPrLookup, String> {
            // #7275 round 2: the answer depends on WHICH branch is asked
            // about, because the guard now asks a second time for the
            // round-stripped stem.
            if self.branch.as_deref() == Ok(branch) {
                return self.merged.clone();
            }
            self.merged_related.clone().unwrap_or_else(|| Ok(lookup(0)))
        }
        fn content_on_base(&self, _dir: &Path, base_ref: &str) -> Result<ContentOnBase, String> {
            *self.asked_base.borrow_mut() = Some(base_ref.to_string());
            // #8633 critic round: the verdict verbatim, so every variant —
            // `Landed { at: Some }` and `Conflicted` included — reaches the guard.
            self.on_base.clone()
        }
        fn landing_admission(&self, _dir: &Path) -> LandingAdmission {
            self.asked_fresh.set(Some(false));
            LandingAdmission {
                content: self.landed.clone(),
                carried: self.carried.clone(),
            }
        }
        fn landing_admission_on_fetched_refs(&self, dir: &Path) -> LandingAdmission {
            let answer = self.landing_admission(dir);
            self.asked_fresh.set(Some(true));
            answer
        }
        fn nested_dirt(&self, _dir: &Path) -> Result<Option<String>, String> {
            self.nested.clone()
        }
    }

    /// The nested clone the #7889 critic round 2 found the guard blind to.
    const NESTED: &str = "nested git worktree/repository `scratch/side-project` holds unsaved \
                          work that `git status` on this directory cannot see: 1 unpushed commit";

    /// 🔴 REGRESSION (#7889 critic round 2): a landed donor tree holding an
    /// ignored nested clone with an unpushed commit is refused, and the deny
    /// names the nested path. `git worktree remove --force` would delete it.
    ///
    /// Fails at 548bc9626, which granted on landed content alone.
    #[test]
    fn worktree_7889_nested_dirt_denies_a_landed_grant() {
        let probe = FakeProbe {
            nested: Ok(Some(NESTED.to_string())),
            ..donor_branch_landed()
        };
        let reason = evaluate_removal_rechecks(Path::new(WT), Ok(&[]), &probe)
            .expect("nested work must deny the landed-content grant");
        assert!(reason.contains(CHECK_CLEAN_TREE), "{reason}");
        assert!(reason.contains("scratch/side-project"), "{reason}");
    }

    /// 🔴 REGRESSION (#7889 critic round 2): the merged-PR grant is guarded too.
    ///
    /// Fails at 548bc9626, whose merged-PR route counted only `git status`.
    #[test]
    fn worktree_7889_nested_dirt_denies_a_merged_pr_grant() {
        let merged = FakeProbe::upstream_deleted();
        assert_eq!(
            evaluate_removal_rechecks(Path::new(WT), Ok(&[]), &merged),
            None,
            "premise: this merged, clean tree is granted"
        );
        let probe = FakeProbe {
            nested: Ok(Some(NESTED.to_string())),
            ..FakeProbe::upstream_deleted()
        };
        let reason = evaluate_removal_rechecks(Path::new(WT), Ok(&[]), &probe)
            .expect("nested work must deny the merged-PR grant");
        assert!(reason.contains("scratch/side-project"), "{reason}");
    }

    /// 🔴 #7889 critic round 2, ADR-0045: a nested scan that could not run
    /// establishes nothing, so it denies rather than granting.
    #[test]
    fn worktree_7889_an_unanswerable_nested_scan_denies() {
        let probe = FakeProbe {
            nested: Err("nested-repository scan failed: permission denied".to_string()),
            ..donor_branch_landed()
        };
        let reason = evaluate_removal_rechecks(Path::new(WT), Ok(&[]), &probe)
            .expect("an unanswerable scan must deny");
        assert!(reason.contains("permission denied"), "{reason}");
    }

    /// 🔴 #7889: the admission reuses the #7914 `local-only-commits` fetch
    /// only when that probe ANSWERED — its production probe answers only after
    /// a successful fetch. An unanswered count means the refs were never
    /// refreshed, so the admission must fetch for itself.
    #[test]
    fn worktree_7889_the_admission_reuses_the_local_only_fetch_only_when_it_succeeded() {
        let fresh = donor_branch_landed();
        assert_eq!(
            evaluate_removal_rechecks(Path::new(WT), Ok(&[]), &fresh),
            None
        );
        assert_eq!(fresh.asked_fresh.get(), Some(true), "one fetch, reused");

        let stale = FakeProbe {
            local_only: Err("`git fetch --prune origin` did not finish within 3s".to_string()),
            ..donor_branch_landed()
        };
        assert_eq!(
            evaluate_removal_rechecks(Path::new(WT), Ok(&[]), &stale),
            None
        );
        assert_eq!(
            stale.asked_fresh.get(),
            Some(false),
            "a failed #7914 fetch must never be trusted as a refresh"
        );
    }

    /// The #7889 shape: a clean, sole-owned tree holding commits no `origin`
    /// ref reaches, on a branch GitHub has no pull request for — because the
    /// work landed through a sibling's squash — whose content IS on the base.
    fn donor_branch_landed() -> FakeProbe {
        FakeProbe {
            branch: Ok("fix/8351-bridge-session-recovery-critic-r1".to_string()),
            merged: Ok(lookup(0)),
            merged_related: None,
            // The #7914 admission cannot fire: these commits exist only here.
            local_only: Ok(3),
            landed: LandedContent::Landed {
                base: "origin/main".to_string(),
                base_sha: "7df1c383f0a1b2c3d4e5f60718293a4b5c6d7e8f".to_string(),
                landed_at: None,
            },
            ..FakeProbe::upstream_deleted()
        }
    }

    /// 🔴 REGRESSION (#7889): the admission itself. A clean, unowned worktree
    /// whose every file is already on `origin/main` is removable even though
    /// no MERGED pull request carries its branch name — and none ever will,
    /// because the branch was fast-forwarded onto a sibling's head and
    /// squash-merged under that name.
    ///
    /// Owner ruling 2026-09-22. Fails against the pre-#7889 guard, which
    /// returns the `merged-pull-request` deny here and never asks a third
    /// question: nineteen such trees were stuck across 2026-09-21/22.
    #[test]
    fn worktree_7889_a_landed_tree_with_no_merged_pr_is_reclaimable() {
        assert_eq!(
            evaluate_removal_rechecks(Path::new(WT), Ok(&[]), &donor_branch_landed()),
            None,
            "a clean tree holding no content the remote lacks must be reclaimable"
        );
    }

    /// 🔴 REGRESSION (#7889, route (c)): HEAD inside the history of a merged
    /// pull request's head admits even where the content comparison refuses —
    /// a donor whose change the pull request itself later superseded.
    #[test]
    fn worktree_7889_a_head_carried_by_a_merged_pr_is_reclaimable() {
        let probe = FakeProbe {
            landed: LandedContent::Residual {
                base: "origin/main".to_string(),
                first_path: "crates/trusty-mpm/src/daemon/mod.rs".to_string(),
            },
            carried: Some(CarriedByPr::Carried {
                pr: 8328,
                pr_head: "2222222222222222222222222222222222222222".to_string(),
            }),
            ..donor_branch_landed()
        };
        assert_eq!(
            evaluate_removal_rechecks(Path::new(WT), Ok(&[]), &probe),
            None,
            "every commit here was inside what PR #8328 merged"
        );
    }

    /// 🔴 #7889: both content routes failing denies, and the deny names each
    /// predicate that failed — (b) with its first residual path, (c) with what
    /// could not be established.
    #[test]
    fn worktree_7889_both_routes_failing_denies_and_names_each() {
        let probe = FakeProbe {
            landed: LandedContent::Residual {
                base: "origin/main".to_string(),
                first_path: "crates/trusty-mpm/src/daemon/mod.rs".to_string(),
            },
            carried: Some(CarriedByPr::Unavailable {
                detail: "the MERGED pull request search did not answer: gh timed out".to_string(),
            }),
            ..donor_branch_landed()
        };
        let reason = evaluate_removal_rechecks(Path::new(WT), Ok(&[]), &probe)
            .expect("neither route admitting must deny removal");
        assert!(reason.contains(CHECK_LANDED_CONTENT), "{reason}");
        assert!(reason.contains(CHECK_MERGED_PR_ANCESTRY), "{reason}");
        assert!(
            reason.contains("crates/trusty-mpm/src/daemon/mod.rs"),
            "{reason}"
        );
        assert!(reason.contains("gh timed out"), "{reason}");
    }

    /// 🔴 #7889, the refusing direction: one path the merge would still change
    /// is work on no remote, and the deny names that path and the admission.
    #[test]
    fn worktree_7889_a_residual_path_denies_and_names_it() {
        let probe = FakeProbe {
            landed: LandedContent::Residual {
                base: "origin/main".to_string(),
                first_path: "crates/trusty-mpm/src/daemon/mod.rs".to_string(),
            },
            ..donor_branch_landed()
        };
        let reason = evaluate_removal_rechecks(Path::new(WT), Ok(&[]), &probe)
            .expect("work the base does not hold must deny removal");
        assert!(reason.contains(CHECK_MERGED_PULL_REQUEST), "{reason}");
        assert!(reason.contains(CHECK_LANDED_CONTENT), "{reason}");
        assert!(
            reason.contains("crates/trusty-mpm/src/daemon/mod.rs"),
            "the deny must name the first residual path: {reason}"
        );
    }

    /// 🔴 #7889, ADR-0045: the admission is a RELAXATION, so only a positive
    /// answer grants. A refresh that failed, a base that would not resolve and
    /// a `merge-tree` that errored all arrive here as `Unavailable`.
    #[test]
    fn worktree_7889_an_unestablished_landed_content_answer_never_grants() {
        let probe = FakeProbe {
            landed: LandedContent::unavailable(
                "`origin` could not be refreshed, so the remote-tracking refs cannot be \
                 trusted: `git fetch --prune origin` did not finish within 3s",
            ),
            ..donor_branch_landed()
        };
        let reason = evaluate_removal_rechecks(Path::new(WT), Ok(&[]), &probe)
            .expect("an unestablished admission must never grant");
        assert!(reason.contains(CHECK_LANDED_CONTENT), "{reason}");
        assert!(
            reason.contains("could not be refreshed"),
            "the deny must quote what failed: {reason}"
        );
    }

    /// 🔴 #7889: the admission did not become a bypass — `clean-tree` still
    /// runs first, so unsaved work denies however landed the history is.
    #[test]
    fn worktree_7889_a_dirty_tree_denies_even_when_its_content_is_landed() {
        let probe = FakeProbe {
            dirty: Ok(3),
            ..donor_branch_landed()
        };
        let reason = evaluate_removal_rechecks(Path::new(WT), Ok(&[]), &probe)
            .expect("unsaved work must deny removal");
        assert!(reason.contains(CHECK_CLEAN_TREE), "{reason}");
        assert!(reason.contains('3'), "{reason}");
    }

    /// 🔴 #7889: `sole-owner` still runs first too — a tree a live agent is
    /// working in is refused whatever its content looks like.
    #[test]
    fn worktree_7889_a_live_owner_denies_even_when_its_content_is_landed() {
        let owners = ["agent-acb948e25b1025822".to_string()];
        let reason = evaluate_removal_rechecks(Path::new(WT), Ok(&owners), &donor_branch_landed())
            .expect("a live owner must deny removal");
        assert!(reason.contains(CHECK_SOLE_OWNER), "{reason}");
        assert!(reason.contains("agent-acb948e25b1025822"), "{reason}");
    }

    /// 🔴 #7889, the distinction the admission rests on: "GitHub has no such
    /// pull request" is a FACT the admission may be asked after; "the lookup
    /// did not answer" establishes nothing, so the evaluation stops there even
    /// when the content IS landed.
    #[test]
    fn worktree_7889_an_unanswerable_lookup_never_reaches_the_admission() {
        let probe = FakeProbe {
            merged: Err(format!(
                "gh timed out after 20s (repository searched: {FAKE_REPO})"
            )),
            ..donor_branch_landed()
        };
        let reason = evaluate_removal_rechecks(Path::new(WT), Ok(&[]), &probe)
            .expect("an unanswerable lookup must deny removal");
        assert!(reason.contains(CHECK_MERGED_PULL_REQUEST), "{reason}");
        assert!(reason.contains("timed out"), "{reason}");
        assert!(
            !reason.contains(CHECK_LANDED_CONTENT),
            "the admission must not be reached from an unestablished fact: {reason}"
        );
    }

    /// REGRESSION (#7275, round 2): a round-N sibling is reclaimable when its
    /// ROUND-1 pull request merged and its content is on that pull request's
    /// base — no MERGED row ever carries the `-r2` name itself.
    #[test]
    fn a_round_sibling_whose_related_pr_merged_is_reclaimable() {
        assert!(
            evaluate_removal_rechecks(Path::new(WT), Ok(&[]), &FakeProbe::round_sibling())
                .is_none(),
            "a related MERGED pull request plus content on its base IS the landing \
             evidence the merged-PR question stands in for"
        );
    }

    /// 🔴 REGRESSION (#7275, round 2): the critic's CRITICAL. An empty merge
    /// tree with NO merged pull request anywhere must DENY.
    ///
    /// Why: round 1 asked `content_on_base` the moment the lookup
    /// returned zero and granted on `Ok(true)`. A branch that was never pushed,
    /// holding one empty or self-reverting commit, answers that exactly the way
    /// a landed branch does — and it clears every other re-check by
    /// construction: clean by being clean, `unpushed-commits` by reporting
    /// `NoUpstream`, `sole-owner` by holding no live claim. The guard deleted a
    /// tree GitHub had never seen. Fails on the round-1 commit, which grants.
    ///
    /// #7889 (owner ruling 2026-09-22) superseded half of this: a never-pushed
    /// tree may now be admitted — but only by the refreshed `landed-content`
    /// admission, never by the merged-PR route's un-refreshed merge-tree
    /// question, which is still not asked without a pull request.
    #[test]
    fn an_empty_merge_tree_without_any_merged_pr_still_denies() {
        let probe = FakeProbe {
            // The exact failing input: no PR for the branch, none for a
            // sibling, an unpushed branch, and a merge that changes nothing.
            branch: Ok("feat/never-pushed".to_string()),
            merged: Ok(lookup(0)),
            merged_related: None,
            on_base: Ok(tip_landed()),
            ..FakeProbe::upstream_deleted()
        };
        let reason = evaluate_removal_rechecks(Path::new(WT), Ok(&[]), &probe)
            .expect("the merged-PR route's merge-tree answer alone is not landing evidence");
        assert!(reason.contains(CHECK_MERGED_PULL_REQUEST), "{reason}");
        assert!(
            reason.contains(CHECK_LANDED_CONTENT),
            "the deny must name the admission that did not establish landing: {reason}"
        );
        assert_eq!(
            *probe.asked_base.borrow(),
            None,
            "the merge-tree question must not be asked without a merged pull request"
        );
    }

    /// REGRESSION (#7275, round 2): a related MERGED pull request is not a
    /// blanket pass — a sibling still holding residue denies and names it.
    #[test]
    fn a_related_merged_pr_with_a_non_empty_merge_tree_denies_with_the_residue() {
        let probe = FakeProbe {
            on_base: Ok(residual()),
            ..FakeProbe::round_sibling()
        };
        let reason = evaluate_removal_rechecks(Path::new(WT), Ok(&[]), &probe)
            .expect("work the merge did not carry must deny");
        assert!(reason.contains(CHECK_MERGED_PULL_REQUEST), "{reason}");
        assert!(reason.contains("would still change files"), "{reason}");
        assert!(reason.contains("diff --name-only"), "{reason}");
        // #8633: the refusal quotes the probe's own result.
        assert!(reason.contains("Probe result:"), "{reason}");
        assert!(reason.contains("`src/lib.rs`"), "{reason}");
    }

    /// 🔴 REGRESSION (#7275, round 2): the base is the merged pull request's
    /// own `baseRefName`, never `origin/HEAD`.
    ///
    /// Why: `base_ref_for` resolved `origin/HEAD` (falling back to
    /// `origin/main`), so a branch that merged into a release or stacked base
    /// was judged against the default branch — the wrong comparison, in the
    /// grant direction. Fails on the round-1 commit, where the probe is never
    /// told which base to use.
    #[test]
    fn the_merge_tree_is_judged_against_the_merged_prs_own_base() {
        let probe = FakeProbe {
            merged_related: Some(Ok(lookup_on(1, "release/2.0"))),
            ..FakeProbe::round_sibling()
        };
        assert!(
            evaluate_removal_rechecks(Path::new(WT), Ok(&[]), &probe).is_none(),
            "content on the PR's own base is still landing evidence"
        );
        assert_eq!(
            probe.asked_base.borrow().as_deref(),
            Some("origin/release/2.0"),
            "the merge-tree question must use the pull request's base, not origin/HEAD"
        );
    }

    /// #7275 round 2: a merged pull request GitHub named no base for is
    /// undeterminable, not a licence to pick one.
    #[test]
    fn a_merged_pr_with_no_base_ref_denies() {
        let probe = FakeProbe {
            merged_related: Some(Ok(lookup_on(1, ""))),
            ..FakeProbe::round_sibling()
        };
        let reason = evaluate_removal_rechecks(Path::new(WT), Ok(&[]), &probe)
            .expect("a base that cannot be established denies");
        assert!(reason.contains("named no base branch"), "{reason}");
        assert!(
            probe.asked_base.borrow().is_none(),
            "the merge-tree question must not be asked without a base"
        );
    }

    /// REGRESSION (#7275): the standard post-merge state — `gh pr merge
    /// --delete-branch` deleted the remote branch, so the stale tracking ref
    /// leaves HEAD reading as "1 commit not on upstream" — no longer refuses a
    /// tree whose content is on the base. Five clean, merged trees were blocked
    /// this way on 2026-09-09, which is the exact cleanup the owner ruled must
    /// happen. Fails on `origin/main`, where `Ahead(n > 0)` denies outright.
    #[test]
    fn a_stale_upstream_no_longer_refuses_a_merged_tree() {
        let probe = FakeProbe {
            unpushed: Ok(UpstreamComparison::Ahead(1)),
            on_base: Ok(tip_landed()),
            ..FakeProbe::reclaimable()
        };
        assert!(
            evaluate_removal_rechecks(Path::new(WT), Ok(&[]), &probe).is_none(),
            "a stale tracking ref is not unpushed work when the content is on the base"
        );
    }

    /// #7275: and genuinely unpushed work still denies, naming how to see it.
    #[test]
    fn a_stale_upstream_still_denies_when_work_is_not_on_the_base() {
        let probe = FakeProbe {
            unpushed: Ok(UpstreamComparison::Ahead(1)),
            on_base: Ok(residual()),
            ..FakeProbe::reclaimable()
        };
        let reason = evaluate_removal_rechecks(Path::new(WT), Ok(&[]), &probe)
            .expect("real unpushed work must deny");
        assert!(reason.contains(CHECK_UNPUSHED_COMMITS), "{reason}");
        assert!(reason.contains("on no remote"), "{reason}");
        assert!(reason.contains("diff --name-only"), "{reason}");
    }

    /// #7275: an unanswerable content question denies, like every other
    /// undeterminable fact this guard consults.
    #[test]
    fn a_sibling_whose_content_cannot_be_checked_denies() {
        let probe = FakeProbe {
            on_base: Err("git could not be run".to_string()),
            ..FakeProbe::round_sibling()
        };
        assert!(
            evaluate_removal_rechecks(Path::new(WT), Ok(&[]), &probe).is_some(),
            "undeterminable is not absent"
        );
    }

    /// #8633 critic round: content that landed at an EARLIER base commit —
    /// the squash, edited over on `main` since — admits through the guard.
    #[test]
    fn content_landed_at_an_earlier_base_commit_allows() {
        let probe = FakeProbe {
            on_base: Ok(ContentOnBase::Landed {
                at: Some(MERGED_PR_HEAD.to_string()),
            }),
            ..FakeProbe::round_sibling()
        };
        assert_eq!(
            evaluate_removal_rechecks(Path::new(WT), Ok(&[]), &probe),
            None,
            "a history hit is landing evidence"
        );
        assert!(probe.asked_base.borrow().is_some(), "the probe was asked");
    }

    /// 🔴 #8633 critic round: a tip merge that conflicts with no landing
    /// commit in history denies, and the refusal names the conflicted file.
    #[test]
    fn a_conflicted_merge_with_no_landing_commit_denies() {
        let probe = FakeProbe {
            on_base: Ok(ContentOnBase::Conflicted {
                paths: vec!["src/main.rs".into()],
                searched: 3,
                candidates: 3,
            }),
            ..FakeProbe::round_sibling()
        };
        let reason = evaluate_removal_rechecks(Path::new(WT), Ok(&[]), &probe)
            .expect("a conflict with no landing commit must deny");
        assert!(reason.contains(CHECK_MERGED_PULL_REQUEST), "{reason}");
        assert!(reason.contains("conflicts in 1 file(s)"), "{reason}");
        assert!(reason.contains("`src/main.rs`"), "{reason}");
        assert!(reason.contains("none of the 3 commit(s)"), "{reason}");
    }

    /// 🔴 #8633 round 3: an empty tip merge whose branch undid part of what
    /// landed denies, and says so instead of claiming the merge still changes
    /// files.
    #[test]
    fn an_undone_landing_denies_and_names_what_was_taken_back() {
        let probe = FakeProbe {
            on_base: Ok(ContentOnBase::Undone {
                at: MERGED_PR_HEAD.to_string(),
                paths: vec!["added.txt".into()],
                searched: 1,
                candidates: 1,
            }),
            ..FakeProbe::round_sibling()
        };
        let reason = evaluate_removal_rechecks(Path::new(WT), Ok(&[]), &probe)
            .expect("an undone landing must deny");
        assert!(reason.contains(CHECK_MERGED_PULL_REQUEST), "{reason}");
        assert!(
            reason.contains("no longer holds all of what landed"),
            "{reason}"
        );
        assert!(reason.contains("`added.txt`"), "{reason}");
        assert!(!reason.contains("would still change files"), "{reason}");
    }

    /// 🔴 #8633 critic round: a history walk cut short by `MAX_CANDIDATES`
    /// (24) denies, and says the search was not exhaustive.
    #[test]
    fn an_exhausted_candidate_cap_denies_and_says_it_was_truncated() {
        let probe = FakeProbe {
            on_base: Ok(ContentOnBase::Conflicted {
                paths: vec!["src/main.rs".into()],
                searched: 24,
                candidates: 40,
            }),
            ..FakeProbe::round_sibling()
        };
        let reason = evaluate_removal_rechecks(Path::new(WT), Ok(&[]), &probe)
            .expect("a truncated search must deny");
        assert!(reason.contains(CHECK_MERGED_PULL_REQUEST), "{reason}");
        assert!(reason.contains("the oldest 24 of 40 commit(s)"), "{reason}");
        assert!(
            reason.contains("the newer 16 were not searched"),
            "{reason}"
        );
    }

    #[test]
    fn denies_worktree_remove_from_a_subagent() {
        let reason = deny_reason(evaluate_worktree_remove_command(
            "git worktree remove --force .claude/worktrees/agent-x",
            true,
            engineer(),
            Path::new("/repo"),
        ));
        assert!(reason.contains("#5791"), "{reason}");
        // #8577: the remedy is a hand-back, not a fleet-wide prune.
        assert!(reason.contains("hand it back"), "{reason}");
    }

    #[test]
    fn allows_worktree_remove_from_the_pm() {
        // The ruling puts the PM in charge of the removal, so the PM's own
        // call — including the throwaway-worktree escape hatch — must pass.
        assert!(is_allow(&evaluate_worktree_remove_command(
            "git worktree remove .claude/worktrees/baseline-1",
            false,
            DispatchIdentity::default(),
            Path::new("/repo"),
        )));
    }

    #[test]
    fn allows_non_remove_worktree_subcommands() {
        for command in [
            "git worktree list",
            "git worktree prune",
            "git worktree lock .claude/worktrees/agent-x",
            "git status",
            "",
        ] {
            assert!(
                is_allow(&evaluate_worktree_remove_command(
                    command,
                    true,
                    engineer(),
                    Path::new("/repo"),
                )),
                "expected allow for: {command}"
            );
            // The grant is scoped to `remove`; `add`/`move`/`lock`/`prune`
            // keep their own rules for `version-control` too.
            assert!(
                is_allow(&evaluate_worktree_remove_command(
                    command,
                    true,
                    version_control(),
                    Path::new("/repo"),
                )),
                "expected allow for version-control: {command}"
            );
        }
    }

    #[test]
    fn denies_a_remove_hidden_in_a_composed_command() {
        // A benign leading verb must not hide the removal, and `-C` must not
        // move it out of range.
        for command in [
            "cargo test -p trusty-mpm && git worktree remove /some/tree",
            "git -C /repo worktree remove --force /repo/.claude/worktrees/agent-x",
            "true; git worktree remove wt",
        ] {
            assert_eq!(
                deny_reason(evaluate_worktree_remove_command(
                    command,
                    true,
                    engineer(),
                    Path::new("/repo"),
                )),
                WORKTREE_REMOVE_DENY_REASON,
                "expected deny for: {command}"
            );
        }
    }

    /// #8439: an unknown git global option cannot hide the removal.
    #[test]
    fn denies_a_remove_behind_an_unknown_git_global_option() {
        let reason = deny_reason(evaluate_worktree_remove_command(
            "git --shallow-file x worktree remove /repo/.claude/worktrees/agent-x",
            true,
            engineer(),
            Path::new("/repo"),
        ));
        assert_eq!(reason, WORKTREE_REMOVE_DENY_REASON);
    }

    #[test]
    fn resolves_the_removal_target_against_a_dash_c_directory() {
        let target = recheck_target(evaluate_worktree_remove_command(
            "git -C /repo worktree remove --force .claude/worktrees/agent-x",
            true,
            version_control(),
            Path::new("/elsewhere"),
        ));
        assert_eq!(target, PathBuf::from(WT));
    }

    /// #7098: `git -C $MAIN worktree remove $MAIN/…` used to reach the
    /// re-checks with `$MAIN` joined twice, so the refusal read as a
    /// `clean-tree` failure for `<repo>/$MAIN/$MAIN/.claude/worktrees/agent-x`
    /// — a directory the command never named and that cannot exist. The
    /// removal is still refused; the refusal now names the variable and quotes
    /// the token as written.
    #[test]
    fn denies_a_removal_whose_path_carries_an_unexpanded_variable() {
        let reason = deny_reason(evaluate_worktree_remove_command(
            "git -C $MAIN worktree remove $MAIN/.claude/worktrees/agent-x",
            true,
            version_control(),
            Path::new("/repo"),
        ));
        assert!(reason.contains(CHECK_WORKTREE_SCOPE), "{reason}");
        assert!(reason.contains("$MAIN"), "{reason}");
        assert!(reason.contains("unresolved shell expansion"), "{reason}");
        assert!(
            !reason.contains("$MAIN/$MAIN"),
            "the doubled join must never reach the message: {reason}"
        );
        assert!(
            !reason.contains(CHECK_CLEAN_TREE),
            "an unresolvable path is not a dirty tree: {reason}"
        );
    }

    /// The variable can sit in the `-C` half alone: the removal path is then
    /// absolute and clean, but the guard still probed nothing real (#7098).
    #[test]
    fn denies_a_removal_whose_dash_c_carries_an_unexpanded_variable() {
        let reason = deny_reason(evaluate_worktree_remove_command(
            "git -C ${MAIN}/sub worktree remove ../.claude/worktrees/agent-x",
            true,
            version_control(),
            Path::new("/repo"),
        ));
        assert!(reason.contains(CHECK_WORKTREE_SCOPE), "{reason}");
        assert!(reason.contains("${MAIN}"), "{reason}");
    }

    /// The expansions `resolve_target_path` DOES perform must not trip the new
    /// refusal — `$PWD` resolves against the tracked base.
    #[test]
    fn allows_a_removal_whose_path_uses_an_expanded_variable() {
        let target = recheck_target(evaluate_worktree_remove_command(
            "git worktree remove --force $PWD/.claude/worktrees/agent-x",
            true,
            version_control(),
            Path::new("/repo"),
        ));
        assert_eq!(target, PathBuf::from(WT));
    }

    #[test]
    fn allows_worktree_remove_from_version_control_on_clean_merged_unowned_tree() {
        // ADR-0057's whole point: the one shape the grant reaches.
        let target = recheck_target(evaluate_worktree_remove_command(
            "git worktree remove --force .claude/worktrees/agent-x",
            true,
            version_control(),
            Path::new("/repo"),
        ));
        assert_eq!(target, PathBuf::from(WT));
        assert_eq!(
            evaluate_removal_rechecks(&target, Ok(&[]), &FakeProbe::reclaimable()),
            None,
            "a clean, pushed, merged, unowned tree must pass every re-check"
        );
    }

    #[test]
    fn denies_worktree_remove_from_version_control_when_tree_dirty() {
        let probe = FakeProbe {
            dirty: Ok(3),
            ..FakeProbe::reclaimable()
        };
        let reason = evaluate_removal_rechecks(Path::new(WT), Ok(&[]), &probe)
            .expect("a dirty tree must deny removal");
        assert!(reason.contains(CHECK_CLEAN_TREE), "{reason}");
        assert!(reason.contains('3'), "{reason}");
    }

    #[test]
    fn denies_worktree_remove_from_version_control_when_commits_are_unpushed() {
        // A clean working tree is not a pushed one. Removing here destroys
        // commits that exist nowhere else, which is the harm `unpushed-commits`
        // is separate from `clean-tree` to catch.
        let probe = FakeProbe {
            unpushed: Ok(UpstreamComparison::Ahead(2)),
            ..FakeProbe::reclaimable()
        };
        let reason = evaluate_removal_rechecks(Path::new(WT), Ok(&[]), &probe)
            .expect("an unpushed commit must deny removal");
        assert!(reason.contains(CHECK_UNPUSHED_COMMITS), "{reason}");
        assert!(reason.contains('2'), "{reason}");
    }

    #[test]
    fn denies_worktree_remove_from_version_control_when_no_merged_pr() {
        let probe = FakeProbe {
            merged: Ok(lookup(0)),
            ..FakeProbe::reclaimable()
        };
        let reason = evaluate_removal_rechecks(Path::new(WT), Ok(&[]), &probe)
            .expect("an unmerged branch must deny removal");
        assert!(reason.contains(CHECK_MERGED_PULL_REQUEST), "{reason}");
        assert!(reason.contains("feat/thing"), "{reason}");
        // #8577: the no-PR detail no longer offers a fleet-wide sweep.
        assert!(!reason.contains("--force"), "{reason}");
    }

    /// 🔴 #7057: the refusal names the repository it searched.
    ///
    /// Why: "no merged pull request" is also what a lookup aimed at the WRONG
    /// repository says. A prune run for `1m-consulting/adaptive-crm` whose `gh`
    /// answered for `hotstats/hotstats-product-poc` produced exactly this deny
    /// for branches whose pull requests had merged, and nothing in the message
    /// could have shown that. Fails on `origin/main`, where the deny names only
    /// the branch.
    #[test]
    fn deny_names_the_repository_the_merged_pr_lookup_searched() {
        let probe = FakeProbe {
            merged: Ok(lookup(0)),
            ..FakeProbe::reclaimable()
        };
        let reason = evaluate_removal_rechecks(Path::new(WT), Ok(&[]), &probe)
            .expect("an unmerged branch must deny removal");
        assert!(
            reason.contains(FAKE_REPO),
            "the deny must name the repository searched: {reason}"
        );
        assert!(reason.contains("origin"), "{reason}");
    }

    #[test]
    fn denies_worktree_remove_from_version_control_when_another_agent_holds_lock() {
        let owners = vec!["rust-engineer".to_string()];
        let reason =
            evaluate_removal_rechecks(Path::new(WT), Ok(&owners), &FakeProbe::reclaimable())
                .expect("a tree another live agent holds must deny removal");
        assert!(reason.contains(CHECK_SOLE_OWNER), "{reason}");
        assert!(reason.contains("rust-engineer"), "{reason}");
    }

    #[test]
    fn denies_worktree_remove_from_version_control_when_the_owner_query_fails() {
        // The critical hole the first cut shipped: the fail-OPEN reader mapped
        // an unreachable daemon to an empty writer set, and an empty set reads
        // as "nobody owns this". Silence establishes nothing, so it must deny.
        let reason = evaluate_removal_rechecks(
            Path::new(WT),
            Err("nothing is listening at http://127.0.0.1:1"),
            &FakeProbe::reclaimable(),
        )
        .expect("an unanswered owner query must deny removal");
        assert!(reason.contains(CHECK_SOLE_OWNER), "{reason}");
        assert!(reason.contains("nothing is listening"), "{reason}");
    }

    #[test]
    fn denies_worktree_remove_when_agent_type_claims_version_control_without_agent_id() {
        // The spoof the grant must not accept: `agent_type` is also stamped on
        // a top-level `--agent version-control` session, so it is never the
        // grant on its own.
        let spoof = DispatchIdentity {
            agent_id: None,
            agent_type: Some("version-control"),
        };
        let reason = deny_reason(evaluate_worktree_remove_command(
            "git worktree remove --force .claude/worktrees/agent-x",
            true,
            spoof,
            Path::new("/repo"),
        ));
        assert!(reason.contains(CHECK_DISPATCH_IDENTITY), "{reason}");
        assert!(reason.contains("agent_id"), "{reason}");
    }

    #[test]
    fn denies_version_control_a_target_outside_a_harness_worktree() {
        let reason = deny_reason(evaluate_worktree_remove_command(
            "git worktree remove --force /repo/some/other/dir",
            true,
            version_control(),
            Path::new("/repo"),
        ));
        assert!(reason.contains(CHECK_WORKTREE_SCOPE), "{reason}");
    }

    /// 🔴 REGRESSION (#8413): a linked worktree in the harness's
    /// `<repo>-worktrees/<tree>` sibling layout reaches the re-checks. Denied
    /// at `worktree-scope` on origin/main. The adjacent case — a MAIN checkout
    /// (`.git` directory) under a `*-worktrees` directory — still denies.
    #[test]
    fn version_control_reaches_the_rechecks_for_a_sibling_layout_worktree() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let linked = tmp.path().join("proj-worktrees").join("agent-a1");
        std::fs::create_dir_all(&linked).expect("mkdir linked");
        std::fs::write(
            linked.join(".git"),
            "gitdir: /proj/.git/worktrees/agent-a1\n",
        )
        .expect("write .git file");
        let command = format!("git worktree remove {}", linked.display());
        let target = recheck_target(evaluate_worktree_remove_command(
            &command,
            true,
            version_control(),
            tmp.path(),
        ));
        assert_eq!(target, linked);

        let main = tmp.path().join("old-worktrees").join("proj");
        std::fs::create_dir_all(main.join(".git")).expect("mkdir main .git");
        let command = format!("git worktree remove {}", main.display());
        let reason = deny_reason(evaluate_worktree_remove_command(
            &command,
            true,
            version_control(),
            tmp.path(),
        ));
        assert!(reason.contains(CHECK_WORKTREE_SCOPE), "{reason}");
    }

    #[test]
    fn denies_version_control_when_a_fact_cannot_be_established() {
        // ADR-0045: undeterminable is never absent on a destructive path.
        // #7232: the `Err` arm now means git could not answer at all — a
        // missing upstream is `UpstreamComparison::NoUpstream`, a fact, and is
        // covered by the four tests below.
        let probe = FakeProbe {
            unpushed: Err("`git rev-parse --verify HEAD` failed: not a git repository".to_string()),
            ..FakeProbe::reclaimable()
        };
        let reason = evaluate_removal_rechecks(Path::new(WT), Ok(&[]), &probe)
            .expect("an unestablished fact must deny removal");
        assert!(reason.contains(CHECK_UNPUSHED_COMMITS), "{reason}");
        assert!(reason.contains("not a git repository"), "{reason}");
    }

    /// 🔴 #7232: the whole bug. `gh pr merge --delete-branch` deletes the remote
    /// branch, so `@{upstream}` stops resolving on every squash-merged
    /// worktree; the guard returned at `unpushed-commits` and never asked
    /// GitHub whether the branch had landed. Fails on `ad64460e8`, where this
    /// denies with `unpushed-commits`.
    #[test]
    fn a_merged_pr_clears_a_worktree_whose_upstream_is_gone() {
        assert_eq!(
            evaluate_removal_rechecks(Path::new(WT), Ok(&[]), &FakeProbe::upstream_deleted()),
            None,
            "a clean tree whose branch has a MERGED pull request must be removable even \
             though the merge deleted its upstream"
        );
    }

    /// The grant is the merged pull request, not the missing upstream. With no
    /// upstream AND no merged pull request nothing establishes the commits ever
    /// reached GitHub, so the removal still denies — and says which branch it
    /// asked about. Fails against an implementation that reads `NoUpstream` as
    /// a pass.
    #[test]
    fn no_upstream_and_no_merged_pr_denies_and_names_the_branch() {
        let probe = FakeProbe {
            merged: Ok(lookup(0)),
            ..FakeProbe::upstream_deleted()
        };
        let reason = evaluate_removal_rechecks(Path::new(WT), Ok(&[]), &probe)
            .expect("no upstream and no merged pull request must deny removal");
        assert!(reason.contains(CHECK_MERGED_PULL_REQUEST), "{reason}");
        assert!(reason.contains("feat/thing"), "{reason}");
        assert!(
            reason.contains("tracks no upstream"),
            "the deny must say the upstream is gone too: {reason}"
        );
    }

    /// ADR-0045 again, at the one gate now load-bearing on its own: a `gh`
    /// lookup that could not answer is never "no merged pull request". The
    /// refusal names the branch, the target and the repository, so the operator
    /// can tell a wrong-repository answer from an absent one (#7057).
    #[test]
    fn no_upstream_and_an_unanswerable_merged_pr_lookup_denies() {
        let probe = FakeProbe {
            merged: Err(format!(
                "gh timed out after 20s (repository searched: {FAKE_REPO})"
            )),
            ..FakeProbe::upstream_deleted()
        };
        let reason = evaluate_removal_rechecks(Path::new(WT), Ok(&[]), &probe)
            .expect("an unanswerable merged-PR lookup must deny removal");
        assert!(reason.contains(CHECK_MERGED_PULL_REQUEST), "{reason}");
        assert!(reason.contains("feat/thing"), "{reason}");
        assert!(reason.contains(WT), "{reason}");
        assert!(reason.contains(FAKE_REPO), "{reason}");
    }

    /// 🔴 The reorder must not have become a bypass: `clean-tree` still runs
    /// FIRST, so unsaved work denies whatever GitHub says about the branch.
    /// Fails against an implementation that lets a merged pull request stand in
    /// for the whole re-check chain.
    #[test]
    fn a_dirty_tree_denies_even_when_merged_with_no_upstream() {
        let probe = FakeProbe {
            dirty: Ok(4),
            ..FakeProbe::upstream_deleted()
        };
        let reason = evaluate_removal_rechecks(Path::new(WT), Ok(&[]), &probe)
            .expect("unsaved work must deny removal");
        assert!(reason.contains(CHECK_CLEAN_TREE), "{reason}");
        assert!(reason.contains('4'), "{reason}");
    }

    /// A tree whose every commit is already on `origin`, with no pull request
    /// anywhere — the #7914 shape.
    fn no_pr_fully_landed() -> FakeProbe {
        FakeProbe {
            branch: Ok("fix/7965-sweep-stall-salvage".to_string()),
            merged: Ok(lookup(0)),
            merged_related: None,
            // Not a route to a grant: content-equivalence alone stays denied
            // (#7275 round 2), so only the local-only count can admit here.
            on_base: Ok(residual()),
            local_only: Ok(0),
            ..FakeProbe::upstream_deleted()
        }
    }

    /// 🔴 REGRESSION (#7914): a clean worktree holding no commit any `origin`
    /// ref lacks is removable with no pull request in evidence at all.
    ///
    /// Why: gate 5 accepted exactly one proof that the commits reached GitHub,
    /// and a branch that never carried a pull request could not produce it —
    /// so an owner's explicit "delete this abandoned tree" had no sanctioned
    /// route from inside a session, on either removal path. The observed shape
    /// is a worktree branched off `origin/main` and never committed to. Fails
    /// on `04c59c7d4`, where this denies with `merged-pull-request`.
    #[test]
    fn a_clean_tree_whose_commits_are_all_on_origin_needs_no_pull_request() {
        assert_eq!(
            evaluate_removal_rechecks(Path::new(WT), Ok(&[]), &no_pr_fully_landed()),
            None,
            "a tree that is the only place for nothing cannot lose anything to a removal"
        );
    }

    /// #7914: a detached HEAD has no branch to look a pull request up by, so
    /// gate 5 could never answer for an install worktree. The admission is
    /// asked before the branch lookup, which is what makes it reachable.
    #[test]
    fn a_detached_head_holding_no_local_only_commit_is_reclaimable() {
        let probe = FakeProbe {
            branch: Err("HEAD is detached — the worktree has no branch to look a \
                         pull request up by"
                .to_string()),
            ..no_pr_fully_landed()
        };
        assert_eq!(
            evaluate_removal_rechecks(Path::new(WT), Ok(&[]), &probe),
            None,
            "a detached HEAD whose commits are all on origin holds nothing to lose"
        );
    }

    /// 🔴 #7914: the admission is a zero, never a "roughly clean". One commit
    /// no `origin` ref has denies exactly as before — this is the #7275 round-2
    /// hole, which must stay shut.
    #[test]
    fn a_commit_no_origin_ref_has_still_denies_without_a_merged_pr() {
        let probe = FakeProbe {
            local_only: Ok(1),
            // The round-2 input verbatim: an empty merge, no pull request.
            on_base: Ok(tip_landed()),
            ..no_pr_fully_landed()
        };
        let reason = evaluate_removal_rechecks(Path::new(WT), Ok(&[]), &probe)
            .expect("a commit that exists only here must deny removal");
        assert!(reason.contains(CHECK_MERGED_PULL_REQUEST), "{reason}");
        assert!(reason.contains(CHECK_LOCAL_ONLY_COMMITS), "{reason}");
        assert!(
            reason.contains("only place they exist"),
            "the deny must name the work at risk: {reason}"
        );
    }

    /// 🔴 #7914: the admission did not become a bypass — `clean-tree` still
    /// runs first, so unsaved work denies however landed the history is.
    #[test]
    fn a_dirty_tree_denies_even_when_no_commit_is_local_only() {
        let probe = FakeProbe {
            dirty: Ok(2),
            ..no_pr_fully_landed()
        };
        let reason = evaluate_removal_rechecks(Path::new(WT), Ok(&[]), &probe)
            .expect("unsaved work must deny removal");
        assert!(reason.contains(CHECK_CLEAN_TREE), "{reason}");
        assert!(reason.contains('2'), "{reason}");
    }

    /// 🔴 #7914: `sole-owner` still runs first too. A tree another live agent
    /// holds is refused whether or not its commits are all on a remote — the
    /// 2026-09-15 recurrence removed a live agent's clean tree.
    #[test]
    fn a_live_owner_denies_even_when_no_commit_is_local_only() {
        let owners = ["agent-a5fc62fa0f4e5ba3d".to_string()];
        let reason = evaluate_removal_rechecks(Path::new(WT), Ok(&owners), &no_pr_fully_landed())
            .expect("a live owner must deny removal");
        assert!(reason.contains(CHECK_SOLE_OWNER), "{reason}");
        assert!(reason.contains("agent-a5fc62fa0f4e5ba3d"), "{reason}");
    }

    /// 🔴 #7914, failure path: the admission fails CLOSED in the only direction
    /// available to a relaxation — a `rev-list` that could not be answered does
    /// not admit, and the deny quotes git's own words rather than reading the
    /// unanswered question as a zero.
    #[test]
    fn an_unanswerable_local_only_count_never_admits() {
        let probe = FakeProbe {
            local_only: Err("`git rev-list` exited 128: not a git repository".to_string()),
            ..no_pr_fully_landed()
        };
        let reason = evaluate_removal_rechecks(Path::new(WT), Ok(&[]), &probe)
            .expect("an unestablished admission must never grant");
        assert!(reason.contains(CHECK_MERGED_PULL_REQUEST), "{reason}");
        assert!(reason.contains("exited 128"), "{reason}");
        assert!(
            reason.contains("could not be established"),
            "the deny must separate an unanswerable count from a real commit: {reason}"
        );
    }

    /// The #7958 shape: a clean tree whose own pull request merged THIS commit,
    /// while `@{upstream}` still resolves and reports the branch 4 ahead.
    fn own_pr_merged_this_head() -> FakeProbe {
        FakeProbe {
            unpushed: Ok(UpstreamComparison::Ahead(4)),
            head_sha: Ok(MERGED_PR_HEAD.to_string()),
            ..FakeProbe::reclaimable()
        }
    }

    /// 🔴 REGRESSION (#7958): a worktree sitting on exactly the commit its own
    /// MERGED pull request carried is removable, whatever the tracking ref says.
    ///
    /// Why: `gh pr merge` leaves `@{upstream}` STALE, not level, so `Ahead(4)`
    /// is what a landed worktree reports. The `is_own && !ahead` short-circuit
    /// therefore never fired, and `content_on_base` — asked next —
    /// reported residue for a tree holding none. PR #7946's worktree, at head
    /// `9c8699fe0`, was refused that way on 2026-09-14. Fails on `983b7a2ae`,
    /// where this denies with `unpushed-commits`.
    #[test]
    fn a_head_sha_matching_the_merged_prs_own_head_grants_despite_a_stale_upstream() {
        assert_eq!(
            evaluate_removal_rechecks(Path::new(WT), Ok(&[]), &own_pr_merged_this_head()),
            None,
            "a worktree whose HEAD IS the merged pull request's head holds nothing that \
             merge did not carry"
        );
    }

    /// 🔴 #7958: the grant is an EXACT match, not "merged and ahead". A tree
    /// ahead of its upstream on some other commit denies exactly as before.
    #[test]
    fn a_head_that_is_not_the_merged_prs_head_still_denies_when_ahead() {
        let probe = FakeProbe {
            head_sha: Ok(WORKTREE_HEAD.to_string()),
            ..own_pr_merged_this_head()
        };
        let reason = evaluate_removal_rechecks(Path::new(WT), Ok(&[]), &probe)
            .expect("work ahead of the upstream on another commit must still deny");
        assert!(reason.contains(CHECK_UNPUSHED_COMMITS), "{reason}");
        assert!(reason.contains("would still change files"), "{reason}");
    }

    /// 🔴 #7958, failure path: a HEAD git could not resolve proves nothing, and
    /// a relaxation that cannot be established never grants (ADR-0045).
    #[test]
    fn an_unanswerable_head_sha_never_grants_an_ahead_worktree() {
        let probe = FakeProbe {
            head_sha: Err("`git rev-parse HEAD` exited 128: ambiguous argument".to_string()),
            ..own_pr_merged_this_head()
        };
        let reason = evaluate_removal_rechecks(Path::new(WT), Ok(&[]), &probe)
            .expect("an unresolvable HEAD must never grant on the head-sha route");
        assert!(reason.contains(CHECK_UNPUSHED_COMMITS), "{reason}");
    }

    /// 🔴 #7958, failure path: GitHub reporting a merged pull request with no
    /// `headRefOid` leaves nothing to compare against, so the pre-#7958
    /// decision stands rather than an empty string matching an empty string.
    #[test]
    fn a_merged_pr_carrying_no_head_sha_never_grants_an_ahead_worktree() {
        let probe = FakeProbe {
            merged: Ok(MergedPrLookup::new(1, FAKE_REPO, "main")),
            head_sha: Ok(String::new()),
            ..own_pr_merged_this_head()
        };
        let reason = evaluate_removal_rechecks(Path::new(WT), Ok(&[]), &probe)
            .expect("two unknown head commits are not a match");
        assert!(reason.contains(CHECK_UNPUSHED_COMMITS), "{reason}");
    }

    /// The #7832 shape: a detached review checkout, clean, holding a commit no
    /// `origin` ref has — so #7914's admission cannot rescue it — parked on the
    /// exact head a merged pull request carried.
    fn detached_on_a_merged_pr_head() -> FakeProbe {
        FakeProbe {
            branch: Err("HEAD is detached — the worktree has no branch to look a \
                         pull request up by"
                .to_string()),
            commit_pr: Ok(commit_lookup(1)),
            ..FakeProbe::upstream_deleted()
        }
    }

    /// 🔴 REGRESSION (#7832): a detached checkout whose commit a MERGED pull
    /// request was opened from is removable.
    ///
    /// Why: gate 5 resolved the pull request by branch NAME, and a detached
    /// checkout has none — so `.claude/worktrees/review-7751` at `02a83032d`
    /// was refused with "HEAD is detached" while PR #7794 had already merged
    /// it, leaving a manual `rm` as the only route. #7914's admission does not
    /// reach it: the merge deleted the branch, so the head commit is on no
    /// `origin` ref and the local-only count is 1. Fails on `983b7a2ae`, where
    /// this denies with `merged-pull-request`.
    #[test]
    fn a_detached_head_that_is_a_merged_prs_own_head_is_reclaimable() {
        assert_eq!(
            evaluate_removal_rechecks(Path::new(WT), Ok(&[]), &detached_on_a_merged_pr_head()),
            None,
            "a commit a merged pull request was opened from is landing evidence whether or \
             not a branch name still points at it"
        );
    }

    /// 🔴 #7832: the commit route is evidence, not an exemption. A detached
    /// checkout no merged pull request was opened from still denies, and the
    /// deny names the commit that was searched for.
    #[test]
    fn a_detached_head_no_merged_pr_carries_still_denies() {
        let probe = FakeProbe {
            commit_pr: Ok(commit_lookup(0)),
            ..detached_on_a_merged_pr_head()
        };
        let reason = evaluate_removal_rechecks(Path::new(WT), Ok(&[]), &probe)
            .expect("a detached commit nothing landed must deny removal");
        assert!(reason.contains(CHECK_MERGED_PULL_REQUEST), "{reason}");
        assert!(reason.contains(WORKTREE_HEAD), "{reason}");
        assert!(reason.contains(FAKE_REPO), "{reason}");
        // #8577: the detached-head detail no longer offers a fleet-wide sweep.
        assert!(!reason.contains("--force"), "{reason}");
    }

    /// 🔴 #7832, critic round: a count the policy cannot corroborate never
    /// grants. GitHub's commit search returns pull requests that merely MENTION
    /// a commit, and one row reporting `count: 1` for a head that is some OTHER
    /// commit must deny — otherwise the whole grant rests on a filter in a
    /// different module that this function cannot see.
    #[test]
    fn a_detached_head_matched_to_a_pull_request_with_another_head_denies() {
        let probe = FakeProbe {
            commit_pr: Ok(MergedPrLookup::new(1, FAKE_REPO, "").with_head_sha(MERGED_PR_HEAD)),
            ..detached_on_a_merged_pr_head()
        };
        let reason = evaluate_removal_rechecks(Path::new(WT), Ok(&[]), &probe)
            .expect("a pull request opened from another commit is not this tree's evidence");
        assert!(reason.contains(CHECK_MERGED_PULL_REQUEST), "{reason}");
        assert!(reason.contains(MERGED_PR_HEAD), "{reason}");
        assert!(
            reason.contains("merely mentions a commit"),
            "the deny must say why the reported pull request did not count: {reason}"
        );
    }

    /// 🔴 #7832, critic round: the same guard with the head commit ABSENT. A
    /// row GitHub named no `headRefOid` for leaves nothing to corroborate, so
    /// two empty strings must not compare equal into a grant.
    #[test]
    fn a_detached_head_matched_to_a_pull_request_with_no_head_denies() {
        let probe = FakeProbe {
            commit_pr: Ok(MergedPrLookup::new(1, FAKE_REPO, "")),
            ..detached_on_a_merged_pr_head()
        };
        let reason = evaluate_removal_rechecks(Path::new(WT), Ok(&[]), &probe)
            .expect("a pull request with no head commit cannot vouch for this tree");
        assert!(reason.contains(CHECK_MERGED_PULL_REQUEST), "{reason}");
        assert!(reason.contains("named no head commit"), "{reason}");
    }

    /// 🔴 #7832, failure path: a commit search that did not answer establishes
    /// nothing, so it denies and quotes what GitHub's own failure said.
    #[test]
    fn an_unanswerable_commit_search_denies_a_detached_head() {
        let probe = FakeProbe {
            commit_pr: Err("`gh pr list` timed out after 10s".to_string()),
            ..detached_on_a_merged_pr_head()
        };
        let reason = evaluate_removal_rechecks(Path::new(WT), Ok(&[]), &probe)
            .expect("an unanswered commit search must never grant");
        assert!(reason.contains(CHECK_MERGED_PULL_REQUEST), "{reason}");
        assert!(reason.contains("timed out"), "{reason}");
    }

    /// 🔴 #7832, failure path: with neither a branch nor a resolvable commit
    /// there is nothing left to look a pull request up by, and the deny says so
    /// rather than reading the silence as an absent pull request.
    #[test]
    fn an_unresolvable_head_sha_denies_a_detached_head() {
        let probe = FakeProbe {
            head_sha: Err("`git rev-parse HEAD` exited 128: unknown revision".to_string()),
            ..detached_on_a_merged_pr_head()
        };
        let reason = evaluate_removal_rechecks(Path::new(WT), Ok(&[]), &probe)
            .expect("an unresolvable HEAD on a detached tree must deny");
        assert!(reason.contains(CHECK_MERGED_PULL_REQUEST), "{reason}");
        assert!(reason.contains("unknown revision"), "{reason}");
        assert!(reason.contains("HEAD is detached"), "{reason}");
    }

    #[test]
    fn reads_the_dispatch_identity_off_a_payload() {
        let payload = serde_json::json!({
            "agent_id": "agent-abc123",
            "agent_type": "version-control",
        });
        assert!(DispatchIdentity::from_payload(&payload).is_permitted_remover());
        let empty = serde_json::json!({ "agent_id": "", "agent_type": "version-control" });
        assert!(!DispatchIdentity::from_payload(&empty).is_permitted_remover());
    }
}
