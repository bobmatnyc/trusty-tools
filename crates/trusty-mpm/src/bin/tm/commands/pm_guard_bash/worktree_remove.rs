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
//! `denies_worktree_remove_from_version_control_when_no_merged_pr`,
//! `denies_worktree_remove_from_version_control_when_another_agent_holds_lock`,
//! `denies_worktree_remove_when_agent_type_claims_version_control_without_agent_id`
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
use super::{PathEnv, resolve_target_path};

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
    let Some(target) = removal_target_path(&tail, &target_dir) else {
        return WorktreeRemoveVerdict::Deny(recheck_deny(
            CHECK_WORKTREE_SCOPE,
            &target_dir,
            "the command names no removable path this guard can resolve, so it cannot establish \
             what would be deleted.",
        ));
    };
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
/// What: the first tail token after `remove` that is not a flag, resolved
/// against `base` through the shared [`resolve_target_path`]. `git worktree
/// remove` takes no option that consumes a value, so skipping every `-`-led
/// token cannot swallow the path.
/// Test: `resolves_the_removal_target_against_a_dash_c_directory`.
fn removal_target_path(tail: &[String], base: &Path) -> Option<PathBuf> {
    let arg = tail
        .iter()
        .skip(1)
        .find(|t| !t.starts_with('-') && !t.is_empty())?;
    Some(resolve_target_path(arg, base, &PathEnv::from_process()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::pm_guard_bash::worktree_remove_rechecks::{
        CHECK_CLEAN_TREE, CHECK_MERGED_PULL_REQUEST, CHECK_SOLE_OWNER, evaluate_removal_rechecks,
    };
    use trusty_mpm::core::worktree_removal_facts::WorktreeRemovalProbe;

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
        unpushed: Result<usize, String>,
        branch: Result<String, String>,
        merged: Result<usize, String>,
    }

    impl FakeProbe {
        /// Clean, pushed, on a branch with one merged pull request.
        fn reclaimable() -> Self {
            Self {
                dirty: Ok(0),
                unpushed: Ok(0),
                branch: Ok("feat/thing".to_string()),
                merged: Ok(1),
            }
        }
    }

    impl WorktreeRemovalProbe for FakeProbe {
        fn dirty_entries(&self, _dir: &Path) -> Result<usize, String> {
            self.dirty.clone()
        }
        fn unpushed_commits(&self, _dir: &Path) -> Result<usize, String> {
            self.unpushed.clone()
        }
        fn branch(&self, _dir: &Path) -> Result<String, String> {
            self.branch.clone()
        }
        fn merged_pull_requests(&self, _dir: &Path, _branch: &str) -> Result<usize, String> {
            self.merged.clone()
        }
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
            evaluate_removal_rechecks(&target, &[], &FakeProbe::reclaimable()),
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
        let reason = evaluate_removal_rechecks(Path::new(WT), &[], &probe)
            .expect("a dirty tree must deny removal");
        assert!(reason.contains(CHECK_CLEAN_TREE), "{reason}");
        assert!(reason.contains('3'), "{reason}");
    }

    #[test]
    fn denies_worktree_remove_from_version_control_when_no_merged_pr() {
        let probe = FakeProbe {
            merged: Ok(0),
            ..FakeProbe::reclaimable()
        };
        let reason = evaluate_removal_rechecks(Path::new(WT), &[], &probe)
            .expect("an unmerged branch must deny removal");
        assert!(reason.contains(CHECK_MERGED_PULL_REQUEST), "{reason}");
        assert!(reason.contains("feat/thing"), "{reason}");
    }

    #[test]
    fn denies_worktree_remove_from_version_control_when_another_agent_holds_lock() {
        let owners = vec!["rust-engineer".to_string()];
        let reason = evaluate_removal_rechecks(Path::new(WT), &owners, &FakeProbe::reclaimable())
            .expect("a tree another live agent holds must deny removal");
        assert!(reason.contains(CHECK_SOLE_OWNER), "{reason}");
        assert!(reason.contains("rust-engineer"), "{reason}");
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
        let probe = FakeProbe {
            unpushed: Err("no upstream configured for HEAD".to_string()),
            ..FakeProbe::reclaimable()
        };
        let reason = evaluate_removal_rechecks(Path::new(WT), &[], &probe)
            .expect("an unestablished fact must deny removal");
        assert!(reason.contains("no upstream configured"), "{reason}");
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
