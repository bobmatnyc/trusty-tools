//! `tm hook --pm-guard` — `EnterWorktree` from a worktree-pinned agent, denied
//! with a message that names both paths (issue #7172).
//!
//! Why: a subagent dispatched with `isolation: "worktree"` was told to switch
//! into a different, already-existing worktree. `EnterWorktree` reported
//! success and moved the agent's working directory, and the harness's own
//! isolation guard stayed pinned to the dispatch-time path. Every command
//! after that was refused — a bare `pwd` included — with "a worktree-isolated
//! agent's commands must run inside its worktree", and `ExitWorktree` was
//! refused too, so there was no way back except guessing at a re-entry into
//! the original tree. A success that wedges the agent is worse than a refusal.
//!
//! **The pin this guard cannot move belongs to the harness, not to `tm`.** The
//! refusal strings above are emitted by the Claude Code binary
//! (`This agent is isolated in the worktree …` is present in the harness and
//! absent from every `trusty-*` binary), and nothing in a `PreToolUse` hook can
//! re-point that state. So the re-pin half of #7172 is not implementable here
//! and this module takes the issue's other option: refuse the switch, and say
//! why, instead of letting the harness report a success that strands the agent.
//! `tm`'s OWN worktree model already follows the move — the delegation tracker
//! rewrites `last_agent_cwd` from each hook event's `cwd` (#6556) — so no
//! `tm`-side state is pinned and none needs re-pinning.
//!
//! What: [`evaluate_enter_worktree`] denies an `EnterWorktree` call when the
//! caller is a subagent AND it is standing in a worktree AND the call would
//! move it out of that worktree. Three arms allow:
//!
//! * the PM, and anything else whose caller context is indeterminate — the
//!   fail-open direction inherited from
//!   [`super::pm_guard_fanout::caller_is_subagent`], for its reasons;
//! * a subagent that is NOT standing in a worktree, whose case is #5799 and is
//!   deliberately untouched here;
//! * re-entering the tree the agent is already in — the ONE recovery an agent
//!   wedged by this bug has, so it must never be blocked.
//!
//! Everything else denies, and the indeterminate directions deny with it: a
//! target that resolves outside `.claude/worktrees/`/`.worktrees/` is refused
//! rather than compared, and so is a create-a-new-tree call (`name` with no
//! `path`), which #5649 forbids an agent regardless of this issue. That is the
//! opposite bias from the fan-out guard next door, and it is affordable for the
//! same reason the ADR-0057 re-checks can afford it: this rule fires only when
//! the caller is already proven to be a subagent standing in a worktree, so a
//! false deny costs one tool call by an agent that has a working tree, while a
//! false allow costs that agent every subsequent command.
//!
//! Test: `denies_a_switch_to_another_worktree`,
//! `denies_a_target_outside_the_worktree_family`,
//! `denies_creating_a_new_worktree_while_pinned`,
//! `resolves_a_relative_target_before_comparing`,
//! `allows_re_entering_the_pinned_worktree`,
//! `allows_the_pm`, `allows_an_agent_outside_a_worktree`,
//! `allows_every_other_tool` below;
//! `pm_guard_denies_enter_worktree_switch_from_a_pinned_subagent` and siblings
//! in `tests/tm_hook_pm_guard.rs` run the stdin→decision→stdout path through
//! the real binary.

use std::path::{Path, PathBuf};

use trusty_mpm::core::project_aliases::worktree_root;

use crate::commands::pm_guard_bash::{PathEnv, resolve_target_path};

/// The harness tool that moves a session or agent into a worktree.
///
/// Why: named once so the guard and its tests cannot drift apart on spelling,
/// and matched case-sensitively — an unrelated tool named `enterworktree` is
/// not this one.
/// What: the `tool_name` Claude Code stamps on the call.
/// Test: `allows_every_other_tool`.
const ENTER_WORKTREE_TOOL: &str = "EnterWorktree";

/// The advice every `EnterWorktree` deny carries, after the two paths.
///
/// Why (#7172): a bare refusal makes the model retry the same call, and the
/// retry is what wedges it. The text states the failure mode the agent would
/// otherwise walk into, names the workaround that actually worked in the
/// incident — branch from the target's tip inside the tree it already has —
/// and names the one recovery for an agent that has ALREADY switched, since
/// that agent has no other move left.
/// What: the tail appended to both deny shapes.
/// Test: `denies_a_switch_to_another_worktree` asserts the workaround and the
/// recovery are both present.
const ENTER_WORKTREE_DENY_TAIL: &str = "A pinned agent that switches trees is left wedged: the harness reports the switch as a \
     success and moves the working directory, but its isolation guard can stay pinned to the \
     dispatch-time path, after which every command — a bare `pwd` included — is refused with \
     \"a worktree-isolated agent's commands must run inside its worktree\", and `ExitWorktree` \
     is refused too. Stay in the worktree you were given: ask the PM for the other tree's base \
     SHA and branch from that SHA here, which is the workaround that worked in the incident. \
     If you have already switched and every command is being refused, call `EnterWorktree` \
     again with `path` set to your own worktree — re-entry into the pinned tree is never \
     blocked. SendMessage is never blocked either — use it to report back.";

/// Classify a `PreToolUse` call for the #7172 worktree-switch rule:
/// `Some(reason)` denies, `None` allows.
///
/// Why: kept pure — caller context arrives as a `bool` and the directory as a
/// `&Path`, so every arm is unit-testable without a process, a daemon, or an
/// env mutation. The resolution of both inputs stays at the single call site in
/// [`super::pm_guard::pm_guard`], where the fan-out guard resolves them too, so
/// the two rules can never disagree about who is calling or from where.
/// What: `None` for any tool that is not [`ENTER_WORKTREE_TOOL`], for a caller
/// that is not a subagent, for a `hook_cwd` that names no worktree, and for a
/// target that resolves into the SAME worktree the caller stands in.
/// `Some(reason)` otherwise — a switch to another tree, a target outside the
/// worktree family, or a `name`-shaped create.
/// Test: the `tests` module below.
pub(crate) fn evaluate_enter_worktree(
    tool_name: &str,
    tool_input: Option<&serde_json::Value>,
    caller_is_subagent: bool,
    hook_cwd: &Path,
) -> Option<String> {
    if tool_name != ENTER_WORKTREE_TOOL || !caller_is_subagent {
        return None;
    }
    // Not standing in a worktree → not pinned to one, so this rule has nothing
    // to protect. That case is #5799 and stays out of scope here.
    let pinned = worktree_root(hook_cwd)?;
    let Some(target) = target_path(tool_input) else {
        // `EnterWorktree { name }` creates a tree, which #5649 reserves to the
        // PM's `isolation: "worktree"` declaration.
        return Some(deny_reason(&pinned, None));
    };
    // #7172: resolve before comparing — `../agent-other` names another tree
    // while still carrying the pinned tree's own prefix as text.
    let resolved = resolve_target_path(&target, hook_cwd, &PathEnv::from_process());
    match worktree_root(&resolved) {
        Some(root) if root == pinned => None,
        Some(root) => Some(deny_reason(&pinned, Some(&root))),
        // Outside `.claude/worktrees/`/`.worktrees/` entirely — refused rather
        // than compared, per this module's fail-closed note.
        None => Some(deny_reason(&pinned, Some(&resolved))),
    }
}

/// The `path` argument of an `EnterWorktree` call, when it has one.
///
/// Why: `path` (switch into an existing tree) and `name` (create a new one) are
/// mutually exclusive in the tool's schema, and only the first can be compared
/// against the pinned tree — so the absent-`path` case must be distinguishable
/// from an empty one rather than collapsing into it.
/// What: `Some(trimmed)` for a non-empty string `path`; `None` for a missing,
/// non-string, or blank one.
/// Test: `denies_creating_a_new_worktree_while_pinned`.
fn target_path(tool_input: Option<&serde_json::Value>) -> Option<String> {
    let raw = tool_input?.get("path")?.as_str()?.trim();
    (!raw.is_empty()).then(|| raw.to_string())
}

/// The `permissionDecisionReason` for a denied `EnterWorktree`.
///
/// Why: the incident's cost was that the agent could not tell what had happened
/// or where it now stood, so both paths are named in the refusal itself rather
/// than left for the agent to reconstruct.
/// What: `target` of `None` is the create-a-tree shape; `Some` names where the
/// switch would have gone. Both end in [`ENTER_WORKTREE_DENY_TAIL`].
/// Test: `denies_a_switch_to_another_worktree`,
/// `denies_creating_a_new_worktree_while_pinned`.
fn deny_reason(pinned: &Path, target: Option<&PathBuf>) -> String {
    let head = match target {
        Some(target) => format!(
            "EnterWorktree denied while worktree-pinned (#7172): this agent is pinned to the \
             worktree {} and this call would move it to {}.",
            pinned.display(),
            target.display()
        ),
        None => format!(
            "EnterWorktree denied while worktree-pinned (#7172, #5649): this agent is pinned to \
             the worktree {} and this call would create a worktree of its own, which is the PM's \
             to declare with `isolation: \"worktree\"`.",
            pinned.display()
        ),
    };
    format!("{head} {ENTER_WORKTREE_DENY_TAIL}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The tree the tests pin the caller to, and a directory inside it.
    const PINNED: &str = "/repo/.claude/worktrees/agent-a";
    const PINNED_SUBDIR: &str = "/repo/.claude/worktrees/agent-a/crates/trusty-mpm";

    fn evaluate(input: serde_json::Value, cwd: &str) -> Option<String> {
        evaluate_enter_worktree(ENTER_WORKTREE_TOOL, Some(&input), true, Path::new(cwd))
    }

    #[test]
    fn denies_a_switch_to_another_worktree() {
        // The #7172 incident itself: pinned to A, pointed at B by the PM.
        let reason = evaluate(
            json!({"path": "/repo/.claude/worktrees/agent-b"}),
            PINNED_SUBDIR,
        )
        .expect("a switch to another worktree must deny");
        assert!(
            reason.contains(PINNED),
            "the deny must name the pin: {reason}"
        );
        assert!(
            reason.contains("/repo/.claude/worktrees/agent-b"),
            "the deny must name the target: {reason}"
        );
        assert!(
            reason.contains("base SHA") && reason.contains("call `EnterWorktree` again"),
            "the deny must carry both the workaround and the recovery: {reason}"
        );
    }

    #[test]
    fn denies_a_target_outside_the_worktree_family() {
        // The fail-closed arm: a main checkout is never an acceptable cwd for a
        // pinned agent, so it is refused rather than compared.
        let reason = evaluate(json!({"path": "/repo"}), PINNED)
            .expect("a target outside .claude/worktrees must deny");
        assert!(reason.contains("/repo"), "{reason}");
    }

    #[test]
    fn denies_creating_a_new_worktree_while_pinned() {
        // `name` with no `path` creates a tree — #5649 reserves that to the PM.
        let reason = evaluate(json!({"name": "scratch"}), PINNED)
            .expect("creating a worktree while pinned must deny");
        assert!(reason.contains("#5649"), "{reason}");
        // A blank `path` is the same shape, not an empty comparison.
        assert!(evaluate(json!({"path": "   "}), PINNED).is_some());
        // And so is a call with no input at all.
        assert!(
            evaluate_enter_worktree(ENTER_WORKTREE_TOOL, None, true, Path::new(PINNED)).is_some()
        );
    }

    #[test]
    fn resolves_a_relative_target_before_comparing() {
        // `../agent-b` still carries `agent-a` as text; only normalization
        // shows it names another tree.
        assert!(
            evaluate(json!({"path": "../agent-b"}), PINNED).is_some(),
            "a relative escape into a sibling tree must deny"
        );
        // The mirror: a relative path back to the pinned tree's own root.
        assert_eq!(evaluate(json!({"path": "../.."}), PINNED_SUBDIR), None);
    }

    #[test]
    fn allows_re_entering_the_pinned_worktree() {
        // The one recovery a wedged agent has. Asserted from the tree root and
        // from a subdirectory of it, since both are "the same tree".
        assert_eq!(evaluate(json!({"path": PINNED}), PINNED), None);
        assert_eq!(evaluate(json!({"path": PINNED}), PINNED_SUBDIR), None);
        assert_eq!(
            evaluate(json!({"path": PINNED_SUBDIR}), PINNED),
            None,
            "a subdirectory of the pinned tree is still the pinned tree"
        );
    }

    #[test]
    fn allows_the_pm() {
        // The PM is not pinned and must keep every worktree move it has.
        assert_eq!(
            evaluate_enter_worktree(
                ENTER_WORKTREE_TOOL,
                Some(&json!({"path": "/repo/.claude/worktrees/agent-b"})),
                false,
                Path::new(PINNED),
            ),
            None
        );
    }

    #[test]
    fn allows_an_agent_outside_a_worktree() {
        // A subagent standing in a main checkout is #5799's case, not this
        // one — this rule fires only on a pin it can actually name.
        assert_eq!(
            evaluate(json!({"path": "/repo/.claude/worktrees/agent-b"}), "/repo"),
            None
        );
    }

    #[test]
    fn allows_every_other_tool() {
        // The rule is keyed to one tool name, case-sensitively.
        for tool in ["Bash", "Read", "Edit", "ExitWorktree", "enterworktree"] {
            assert_eq!(
                evaluate_enter_worktree(
                    tool,
                    Some(&json!({"path": "/repo/.claude/worktrees/agent-b"})),
                    true,
                    Path::new(PINNED),
                ),
                None,
                "{tool} must not be touched by this rule"
            );
        }
    }
}
