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
//! `denies_a_removal_whose_dash_c_carries_an_unexpanded_variable`
//! below; `pm_guard_denies_worktree_remove_from_native_subagent` and
//! `pm_guard_allows_worktree_remove_from_pm` run the binary end to end in
//! `tests/tm_hook_pm_guard.rs`.

use std::path::{Path, PathBuf};

use trusty_mpm::core::dispatch_isolation::permitted_in_shared_checkout;
use trusty_mpm::core::project_aliases::is_worktree_path;

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
/// as the obvious next thing to try. The text also names the ruling, the one
/// session allowed to run the removal, the exact command that does it, and
/// what the agent should do instead — report and stop. It says which worktree
/// verbs still work, so an agent reading a registry does not treat the whole
/// subcommand as blocked. Since ADR-0057 it also names the one role the deny
/// no longer reaches, so an agent that has seen `version-control` do this does
/// not read its own deny as a bug.
/// What: the `permissionDecisionReason` string emitted on this deny.
/// Test: `denies_worktree_remove_from_a_subagent`.
pub(crate) const WORKTREE_REMOVE_DENY_REASON: &str = "Worktree removal is PM-executed (#5791, owner ruling 2026-08-19): an agent never removes a \
     worktree, its own included. Report back instead — name the merged PR and the worktree path, \
     then stop. The PM confirms the work is done and reclaims the tree with \
     `tm session prune-worktrees --merged-prs --force`, which spares any worktree still holding \
     unsaved work or still owned by a live agent. `rm -rf` on the worktree directory is not the \
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
    if !is_worktree_path(&target) {
        return WorktreeRemoveVerdict::Deny(recheck_deny(
            CHECK_WORKTREE_SCOPE,
            &target,
            "the target is not under a harness worktree root (`.claude/worktrees/` or \
             `.worktrees/`), and the grant reaches no other directory.",
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
        CHECK_CLEAN_TREE, CHECK_MERGED_PULL_REQUEST, CHECK_SOLE_OWNER, CHECK_UNPUSHED_COMMITS,
        evaluate_removal_rechecks,
    };
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
        /// #7275: whether merging this tree into its base would change nothing.
        noop_merge: Result<bool, String>,
        /// #7275 round 2: the base ref the merge-tree question was asked
        /// against, so a test can prove it came from the pull request.
        asked_base: std::cell::RefCell<Option<String>>,
    }

    /// A merged-PR answer for the fixture repository (#7057), landing on `main`.
    fn lookup(count: usize) -> MergedPrLookup {
        lookup_on(count, if count == 0 { "" } else { "main" })
    }

    /// A merged-PR answer that landed on a named base (#7275 round 2).
    fn lookup_on(count: usize, base: &str) -> MergedPrLookup {
        MergedPrLookup::new(count, FAKE_REPO, base)
    }

    /// The repository the fake probe reports having searched (#7057).
    const FAKE_REPO: &str = "1m-consulting/adaptive-crm";

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
                noop_merge: Ok(false),
                asked_base: std::cell::RefCell::new(None),
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
                noop_merge: Ok(true),
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
        fn merge_into_base_is_a_noop(&self, _dir: &Path, base_ref: &str) -> Result<bool, String> {
            *self.asked_base.borrow_mut() = Some(base_ref.to_string());
            self.noop_merge.clone()
        }
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
    /// Why: round 1 asked `merge_into_base_is_a_noop` the moment the lookup
    /// returned zero and granted on `Ok(true)`. A branch that was never pushed,
    /// holding one empty or self-reverting commit, answers that exactly the way
    /// a landed branch does — and it clears every other re-check by
    /// construction: clean by being clean, `unpushed-commits` by reporting
    /// `NoUpstream`, `sole-owner` by holding no live claim. The guard deleted a
    /// tree GitHub had never seen. Fails on the round-1 commit, which grants.
    #[test]
    fn an_empty_merge_tree_without_any_merged_pr_still_denies() {
        let probe = FakeProbe {
            // The exact failing input: no PR for the branch, none for a
            // sibling, an unpushed branch, and a merge that changes nothing.
            branch: Ok("feat/never-pushed".to_string()),
            merged: Ok(lookup(0)),
            merged_related: None,
            noop_merge: Ok(true),
            ..FakeProbe::upstream_deleted()
        };
        let reason = evaluate_removal_rechecks(Path::new(WT), Ok(&[]), &probe)
            .expect("content-equivalence alone is not landing evidence");
        assert!(reason.contains(CHECK_MERGED_PULL_REQUEST), "{reason}");
        assert!(
            reason.contains("never pushed"),
            "the deny must say why an empty merge proves nothing: {reason}"
        );
    }

    /// REGRESSION (#7275, round 2): a related MERGED pull request is not a
    /// blanket pass — a sibling still holding residue denies and names it.
    #[test]
    fn a_related_merged_pr_with_a_non_empty_merge_tree_denies_with_the_residue() {
        let probe = FakeProbe {
            noop_merge: Ok(false),
            ..FakeProbe::round_sibling()
        };
        let reason = evaluate_removal_rechecks(Path::new(WT), Ok(&[]), &probe)
            .expect("work the merge did not carry must deny");
        assert!(reason.contains(CHECK_MERGED_PULL_REQUEST), "{reason}");
        assert!(reason.contains("would still change files"), "{reason}");
        assert!(reason.contains("diff --name-only"), "{reason}");
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
            noop_merge: Ok(true),
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
            noop_merge: Ok(false),
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
            noop_merge: Err("git could not be run".to_string()),
            ..FakeProbe::round_sibling()
        };
        assert!(
            evaluate_removal_rechecks(Path::new(WT), Ok(&[]), &probe).is_some(),
            "undeterminable is not absent"
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
        assert!(reason.contains("tm session prune-worktrees"), "{reason}");
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
