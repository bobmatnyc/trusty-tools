//! `tm hook --pm-guard` — agent-side `git worktree remove` denial (#5791).
//!
//! Why: removing a merged worktree could not be delegated at all. An
//! unisolated dispatch is denied by the #4480 shared-HEAD guard, and an
//! isolated agent's git operations are confined to its own worktree, so it
//! cannot act on the shared registry either. The owner ruled on 2026-08-19
//! that this is not a hole to open: worktree removal is PM-executed via a `tm`
//! command — the PM confirms the agent's work is done and runs the removal
//! itself. Prose alone does not carry that. `BASE-AGENT.md` told every agent
//! for months to "remove your worktree" after a merge, and an agent that reads
//! a stale copy of that instruction still reaches for `git worktree remove`.
//! What: [`evaluate_worktree_remove_command`] denies a `git worktree remove`
//! segment when, and only when, the caller is a subagent. `list`, `prune`,
//! `lock`, and `move` are untouched — this rule is about destroying a checkout,
//! not about reading or repairing the registry. The PM is never denied: it is
//! the session the ruling puts in charge, and it runs the reclaim through
//! `tm session prune-worktrees --merged-prs --force`.
//!
//! Caller context comes from [`super::super::pm_guard_fanout::caller_is_subagent`],
//! which FAILS OPEN — an indeterminate context reads as "not a subagent" and
//! allows. That asymmetry is deliberate there and inherited here: a false DENY
//! would land on the PM, the one session that must keep working.
//!
//! Test: `denies_worktree_remove_from_a_subagent`,
//! `allows_worktree_remove_from_the_pm`,
//! `allows_non_remove_worktree_subcommands`,
//! `denies_a_remove_hidden_in_a_composed_command` below;
//! `pm_guard_denies_worktree_remove_from_native_subagent` and
//! `pm_guard_allows_worktree_remove_from_pm` run the binary end to end in
//! `tests/tm_hook_pm_guard.rs`.

use super::shell_lex;

/// Deny reason for an agent-side `git worktree remove` (#5791).
///
/// Why: a bare refusal makes the model retry or hand-roll a `rm -rf`. The text
/// names the ruling, the one session allowed to run the removal, the exact
/// command that does it, and what the agent should do instead — report and
/// stop. It also says which worktree verbs still work, so an agent reading a
/// registry does not treat the whole subcommand as blocked.
/// What: the `permissionDecisionReason` string emitted on this deny.
/// Test: `denies_worktree_remove_from_a_subagent`.
pub(crate) const WORKTREE_REMOVE_DENY_REASON: &str = "Worktree removal is PM-executed (#5791, owner ruling 2026-08-19): an agent never removes a \
     worktree, its own included. Report back instead — name the merged PR and the worktree path, \
     then stop. The PM confirms the work is done and reclaims the tree with \
     `tm session prune-worktrees --merged-prs --force`, which spares any worktree still holding \
     unsaved work or still owned by a live agent. `git worktree list` and `git worktree prune` \
     are not blocked, and SendMessage is never blocked — use it to report the path back.";

/// Classify a Bash command for agent-side worktree removal: `Some(reason)`
/// denies, `None` allows.
///
/// Why: kept pure — it takes the already-resolved caller context as a `bool`
/// rather than reading the environment — so the policy is exhaustively unit
/// testable and the one env read stays in `pm_guard_fanout`.
/// What: allows outright when the caller is not a subagent, and otherwise
/// denies when any composition segment is a `git worktree remove`.
/// Test: the four cases named in the module docs.
pub(crate) fn evaluate_worktree_remove_command(
    command: &str,
    caller_is_subagent: bool,
) -> Option<&'static str> {
    if !caller_is_subagent {
        return None;
    }
    command_removes_a_worktree(command).then_some(WORKTREE_REMOVE_DENY_REASON)
}

/// True when any composition segment of `command` is a `git worktree remove`.
///
/// Why: a forbidden verb hides in any segment, not just the first
/// (`cargo test && git worktree remove …`), which is why this reuses the same
/// [`super::split_shell_segments`] the rest of the classifier does rather than
/// matching the whole string.
/// What: splits, keeps the segments whose resolved program is `git` with
/// subcommand `worktree` (so `git -C <path> worktree remove` and an
/// env-prefixed spelling both resolve), and looks for the adjacent
/// `worktree remove` token pair. Same residual bypasses as the sibling
/// `worktree add` guard: a verb built by variable expansion or hidden in a
/// command substitution is not resolved.
fn command_removes_a_worktree(command: &str) -> bool {
    for segment in super::split_shell_segments(command) {
        let trimmed = segment.trim();
        if shell_lex::git_subcommand(trimmed).as_deref() != Some("worktree") {
            continue;
        }
        // `git_subcommand` already proved this segment shlex-parses; the
        // `else` arm is unreachable in practice and skips conservatively.
        let Some(argv) = shlex::split(trimmed) else {
            continue;
        };
        if argv
            .windows(2)
            .any(|w| w[0] == "worktree" && w[1] == "remove")
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn denies_worktree_remove_from_a_subagent() {
        let reason = evaluate_worktree_remove_command(
            "git worktree remove --force .claude/worktrees/agent-x",
            true,
        )
        .expect("a subagent's worktree removal must deny");
        assert!(reason.contains("#5791"), "{reason}");
        assert!(reason.contains("tm session prune-worktrees"), "{reason}");
    }

    #[test]
    fn allows_worktree_remove_from_the_pm() {
        // The ruling puts the PM in charge of the removal, so the PM's own
        // call — including the throwaway-worktree escape hatch — must pass.
        assert_eq!(
            evaluate_worktree_remove_command(
                "git worktree remove .claude/worktrees/baseline-1",
                false
            ),
            None
        );
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
            assert_eq!(
                evaluate_worktree_remove_command(command, true),
                None,
                "expected allow for: {command}"
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
                evaluate_worktree_remove_command(command, true),
                Some(WORKTREE_REMOVE_DENY_REASON),
                "expected deny for: {command}"
            );
        }
    }
}
