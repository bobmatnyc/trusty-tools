//! Give a dispatched writer its own worktree when the session is standing in a
//! main checkout ([ADR-0048](../../../../../../docs/adr/0048-dispatched-writers-get-a-worktree-and-the-write-boundary-is-enforced.md)).
//!
//! Why: ADR-0044 made the main checkout read-only for the PM and for every
//! agent it dispatches, and ADR-0037 made the main checkout where a session
//! runs by default. Together those two leave a dispatched writer with nowhere
//! to write: `worktree_enabled_for_project` never had a production caller, so
//! nothing ever gave an agent a tree of its own, and the agent wrote to the
//! shared one because that is the only one it had. The reported harm is three
//! sessions in one `mcp-services` checkout with branches switching under each
//! other — one commit landing on a branch belonging to a different workstream,
//! and the branch it should have landed on left empty.
//!
//! What: [`evaluate_worktree_grant`] turns an unisolated dispatch into an
//! isolated one instead of refusing it. When the PM is standing in a main
//! checkout and the dispatch [`requires_own_worktree_in_main_checkout`], this
//! returns the dispatch's own `tool_input` with `isolation: "worktree"` added,
//! which `pm_guard` prints as `hookSpecificOutput.updatedInput` — the field
//! Claude Code documents as replacing a tool's arguments before it runs. The
//! harness then creates the worktree under `.claude/worktrees/` per ADR-0036
//! and removes it on completion. Trusty-mpm still creates nothing, which is
//! what ADR-0044 decision 4 requires: this asks the owner of worktrees for one.
//!
//! **A grant, not a deny, because a deny cannot be applied by the party that
//! receives it.** A refusal reaches the PM as text and depends on the model
//! re-issuing the same dispatch with one more field. The rewrite reaches Claude
//! Code as data and is applied whether or not anything read it. The costs are
//! also not equal: a false grant costs one worktree the harness reclaims when
//! it is unchanged, while a false allow puts a second writer on a git HEAD that
//! another session is standing on.
//!
//! **Only the `Agent` tool is rewritten.** `isolation` is an `Agent` parameter;
//! `Task` is the older spelling and does not carry one, and injecting a field a
//! tool's schema rejects would turn a guarded dispatch into a failed tool call.
//! A `Task` dispatch that needs isolation is DENIED instead, and told which
//! tool can give it — see [`TASK_DENY_REASON`].
//!
//! **Fail-open arms, restated because this one differs from its neighbours.**
//! A cwd that is not a main checkout is not this rule's business, and every
//! worktree answers that way, so delegated work in a worktree is untouched. A
//! cwd that resolves to no git checkout at all is likewise untouched. What does
//! NOT fail open here is an unknown agent or an untyped dispatch: both reach
//! [`requires_own_worktree_in_main_checkout`] as indeterminate and are granted a
//! worktree, because a custom or renamed agent writing into the shared checkout
//! is the reported defect rather than a hypothetical one. Granting an
//! unnecessary worktree to a read-only custom agent is the whole cost of that
//! choice.
//!
//! **A project may decline the grant entirely (#5814).** Isolation buys
//! separation between concurrent writers and between build states. A writing or
//! documentation repo has neither, so the grant costs it agent edits stranded in
//! `.claude/worktrees/agent-<id>/` and trees left to reclaim. Such a project sets
//! `agent_worktree = false` in its committed `.trusty-mpm.toml`, which
//! [`trusty_mpm::project::dispatched_agent_worktree_enabled`] reads: the dispatch
//! then gets [`WorktreeGrant::InPlace`] — its prompt annotated with
//! `IN_PLACE_WORKFLOW_NOTE` and no `isolation` added — instead of a worktree.
//! The check runs AFTER `is_main_checkout` so only a dispatch that would
//! otherwise be granted pays for the file read, and an absent, malformed, or
//! unreadable config leaves the grant exactly as it was.
//!
//! Test: `grants_*`, `does_not_grant_*`, `task_dispatch_*`, `in_place_*` below;
//! `pm_guard_grants_a_worktree_to_a_writer_in_a_main_checkout` and siblings in
//! `tests/tm_hook_pm_guard.rs` run it through the real binary.

use std::path::Path;

use serde_json::Value;
use trusty_mpm::core::agent::is_subagent_dispatch_tool;
use trusty_mpm::core::dispatch_isolation::{
    dispatch_agent, dispatch_isolation, requires_own_worktree_in_main_checkout,
};
use trusty_mpm::core::project_aliases::is_main_checkout;
use trusty_mpm::project::dispatched_agent_worktree_enabled;

/// The `isolation` value granted to a dispatch that needs its own tree.
///
/// Why: the harness offers two tree-separating modes and only one of them is
/// this machine's to grant — `remote` runs the agent on different hardware,
/// which is an operator's decision about cost and connectivity, not a guard's.
/// What: the first member of
/// [`ISOLATING_DISPATCH_MODES`](trusty_mpm::core::dispatch_isolation::ISOLATING_DISPATCH_MODES),
/// asserted equal to it
/// so the two cannot drift into disagreeing about what counts as isolation.
/// Test: `granted_mode_is_recognised_as_isolating`.
const GRANTED_ISOLATION: &str = "worktree";

/// The tool whose schema documents an `isolation` parameter.
///
/// Why: see the module doc — a rewrite that adds a field the receiving tool
/// does not accept converts a guarded dispatch into a broken one.
/// What: matched case-sensitively against the payload's `tool_name`.
/// Test: `task_dispatch_is_denied_rather_than_rewritten`.
const ISOLATION_AWARE_DISPATCH_TOOL: &str = "Agent";

/// Deny text for a `Task` dispatch that needs a worktree it cannot be given.
///
/// Why: the one case this module refuses instead of fixing, so the message has
/// to carry the whole remedy — which tool to use, which field to set, and why
/// the directory is special. A constant rather than a built string because
/// nothing about it varies with the dispatch.
/// Test: `task_dispatch_is_denied_rather_than_rewritten`.
pub(crate) const TASK_DENY_REASON: &str = "Unisolated dispatch denied in a main checkout (ADR-0044): this session is standing in a \
     project's main checkout, which is read-only apart from documents and configuration, and \
     this dispatch would put an agent that may write files into it. Another session may be \
     standing in the same directory — the reported failure is a commit landing on a branch \
     belonging to a different workstream, with no error at any step. Dispatch through the \
     `Agent` tool with `isolation: \"worktree\"` instead, which gets the agent a tree of its \
     own; `Task` carries no isolation parameter, so it cannot be given one here. A read-only \
     agent (research, review, analysis) is never blocked by this rule.";

/// What an opted-out project's dispatch is told instead of being isolated.
///
/// Why (#5814): suppressing the grant is only half the fix. The agent still
/// arrives carrying the framework's own worktree discipline and, finding itself
/// in a main checkout with no tree of its own, stops rather than proceeds — the
/// reported failure is a `version-control` agent that refused to commit for
/// exactly that reason. The note states the workflow the project chose when it
/// set the flag, and it reaches the agent as data: it is appended to the
/// dispatch's own prompt through `hookSpecificOutput.updatedInput`, the same
/// mechanism the grant uses, so it applies whether or not the PM read anything.
/// What: appended verbatim, once, to a string `prompt`.
/// Test: `in_place_notice_is_appended_to_the_prompt`,
/// `in_place_notice_is_not_appended_twice`.
const IN_PLACE_WORKFLOW_NOTE: &str = "\n\n---\nWorktree isolation is OFF for this project: it sets `agent_worktree = false` in \
     `.trusty-mpm.toml` (trusty-tools#5814). No worktree has been created for you and none will \
     be reclaimed, so work IN PLACE in the current working directory — edit the files where they \
     are, commit on the branch the checkout already has, and push. Do not create a worktree, a \
     branch, or a pull request unless this project's own instructions ask for one. The \
     main-checkout write boundary is unchanged: a source-file edit here is still denied.";

/// Decide what to do with one dispatch made from a main checkout.
///
/// Why: the single entry point `pm_guard` calls, kept as one function so the
/// ordering — cheapest test first, filesystem last — cannot be re-derived
/// differently at the call site.
/// What: `None` (nothing to do) for every non-dispatch tool, for a dispatch
/// that already declares isolation, for a positively-identified read-only
/// agent, and for any cwd that is not a main checkout. Otherwise
/// [`WorktreeGrant::Rewrite`] carrying the dispatch's own input with
/// `isolation` added, [`WorktreeGrant::Deny`] when the tool cannot accept one,
/// or [`WorktreeGrant::InPlace`] when the project declined isolation (#5814).
///
/// `is_main_checkout` is tested LAST because it is the only branch that touches
/// the filesystem, and every ordinary tool call must not pay for it. The #5814
/// project opt-out reads a second file and therefore sits behind it.
/// Test: `grants_a_worktree_to_an_unisolated_writer`,
/// `does_not_grant_outside_a_main_checkout`, `does_not_grant_a_read_only_agent`,
/// `grants_nothing_when_the_project_opts_out`.
pub(crate) fn evaluate_worktree_grant(
    tool_name: &str,
    tool_input: Option<&Value>,
    cwd: &Path,
) -> Option<WorktreeGrant> {
    if !is_subagent_dispatch_tool(tool_name) {
        return None;
    }
    let agent = dispatch_agent(tool_input).unwrap_or_default();
    if !requires_own_worktree_in_main_checkout(agent, dispatch_isolation(tool_input)) {
        return None;
    }
    if !is_main_checkout(cwd) {
        return None;
    }
    // #5814: a project with no concurrent writers and no build state declares
    // `agent_worktree = false`, and its agents work in the checkout instead.
    if !dispatched_agent_worktree_enabled(cwd) {
        return with_in_place_notice(tool_input).map(WorktreeGrant::InPlace);
    }
    if tool_name != ISOLATION_AWARE_DISPATCH_TOOL {
        return Some(WorktreeGrant::Deny(TASK_DENY_REASON));
    }
    Some(WorktreeGrant::Rewrite(with_granted_isolation(tool_input)))
}

/// What [`evaluate_worktree_grant`] decided.
///
/// Why: the outcomes are different JSON objects on the hook's stdout — two
/// replace the tool's arguments, one blocks the call — and a `PreToolUse` hook
/// may print exactly one of them, so the choice has to be explicit rather than
/// inferred from an `Option` at the call site.
/// Test: `task_dispatch_is_denied_rather_than_rewritten`,
/// `grants_nothing_when_the_project_opts_out`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WorktreeGrant {
    /// Replace the dispatch's arguments with these, isolation included.
    Rewrite(Value),
    /// Block the dispatch; the tool cannot be given isolation.
    Deny(&'static str),
    /// The project declined isolation (#5814): these arguments carry the
    /// original input plus the in-place workflow note, and NO `isolation`.
    ///
    /// It is a separate variant from [`Self::Rewrite`] because the call site
    /// must not report a granted worktree to the daemon for a dispatch that was
    /// given none — the delegation record would then name a tree that does not
    /// exist. The concurrency question is still asked; see the call site.
    InPlace(Value),
}

/// The dispatch's own input with `isolation` set to [`GRANTED_ISOLATION`].
///
/// Why: `updatedInput` REPLACES a tool's arguments rather than merging into
/// them, so the rewrite has to carry every field the caller sent — dropping the
/// prompt would dispatch an agent with no brief.
/// What: clones `tool_input` and inserts `isolation`. A missing or non-object
/// input yields an object holding `isolation` alone: there is nothing to
/// preserve, and the dispatch was already malformed by the time it got here.
/// Test: `rewrite_preserves_every_field_the_caller_sent`.
fn with_granted_isolation(tool_input: Option<&Value>) -> Value {
    let mut input = match tool_input {
        Some(Value::Object(map)) => Value::Object(map.clone()),
        _ => Value::Object(serde_json::Map::new()),
    };
    input["isolation"] = Value::String(GRANTED_ISOLATION.to_string());
    input
}

/// The dispatch's own input with [`IN_PLACE_WORKFLOW_NOTE`] appended to its prompt.
///
/// Why (#5814): `updatedInput` REPLACES a tool's arguments, so the whole input
/// is carried and only the prompt changes — above all, `isolation` is NOT added,
/// which is the entire point of the opt-out.
/// What: `Some(input)` when the dispatch carries a string `prompt` that does not
/// already end with the note; `None` otherwise, which leaves the dispatch
/// untouched and unisolated. `None` covers both a dispatch with no prompt to
/// annotate — there is nothing to say it to, and inventing a field the caller
/// did not send is how a malformed rewrite gets built — and a re-evaluated
/// dispatch, which must not accumulate a second copy of the note.
/// Test: `in_place_notice_is_appended_to_the_prompt`,
/// `in_place_notice_is_not_appended_twice`,
/// `in_place_leaves_a_promptless_dispatch_alone`.
fn with_in_place_notice(tool_input: Option<&Value>) -> Option<Value> {
    let Some(Value::Object(map)) = tool_input else {
        return None;
    };
    let prompt = map.get("prompt").and_then(Value::as_str)?;
    if prompt.contains(IN_PLACE_WORKFLOW_NOTE) {
        return None;
    }
    let mut input = Value::Object(map.clone());
    input["prompt"] = Value::String(format!("{prompt}{IN_PLACE_WORKFLOW_NOTE}"));
    Some(input)
}

/// Build the `hookSpecificOutput.updatedInput` body for a granted dispatch.
///
/// Why: mirrors [`crate::commands::hook_rewrite::build_pretooluse_rewrite_response`]
/// — the same field, confirmed against the same Claude Code hooks reference —
/// rather than reusing it, because that one is shaped for a Bash command string
/// and this one carries a whole tool input.
/// What: `{"hookSpecificOutput": {"hookEventName": "PreToolUse",
/// "updatedInput": <input>}}`. The #5814 in-place rewrite prints through here
/// too — the envelope is the same, only the input differs.
/// Test: `grant_response_has_the_documented_shape`.
pub(crate) fn build_worktree_grant_response(updated_input: &Value) -> String {
    serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "updatedInput": updated_input,
        }
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use trusty_mpm::core::dispatch_isolation::ISOLATING_DISPATCH_MODES;

    /// A directory that answers `is_main_checkout` — a `.git` DIRECTORY, which
    /// is how git marks a main checkout and never a linked worktree.
    fn main_checkout() -> TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(dir.path().join(".git")).expect("mkdir .git");
        dir
    }

    fn input(agent: &str, isolation: Option<&str>) -> Value {
        match isolation {
            Some(i) => serde_json::json!({"subagent_type": agent, "isolation": i}),
            None => serde_json::json!({"subagent_type": agent}),
        }
    }

    #[test]
    fn granted_mode_is_recognised_as_isolating() {
        // The grant is worthless if the mode it injects is not one the sibling
        // classifier accepts — the dispatch would be rewritten and then denied.
        assert_eq!(ISOLATING_DISPATCH_MODES[0], GRANTED_ISOLATION);
        assert!(
            trusty_mpm::core::dispatch_isolation::isolation_separates_working_tree(Some(
                GRANTED_ISOLATION
            ))
        );
    }

    #[test]
    fn grants_a_worktree_to_an_unisolated_writer() {
        let dir = main_checkout();
        let grant =
            evaluate_worktree_grant("Agent", Some(&input("rust-engineer", None)), dir.path())
                .expect("a writer in a main checkout must be granted a worktree");
        let Some(WorktreeGrant::Rewrite(updated)) = Some(grant) else {
            panic!("expected a rewrite");
        };
        assert_eq!(updated["isolation"], "worktree");
        assert_eq!(updated["subagent_type"], "rust-engineer");
    }

    #[test]
    fn grants_a_worktree_to_an_unknown_agent() {
        // ADR-0048's substance: a custom or renamed agent is exactly the writer
        // that kept landing in the shared checkout, and it is indeterminate to
        // every name-based classifier. It is granted, not allowed through.
        let dir = main_checkout();
        for agent in ["some-project-custom-agent", "Rust-Engineer"] {
            assert!(
                matches!(
                    evaluate_worktree_grant("Agent", Some(&input(agent, None)), dir.path()),
                    Some(WorktreeGrant::Rewrite(_))
                ),
                "{agent} is unknown and must be isolated rather than trusted"
            );
        }
        // An untyped dispatch — no `subagent_type` at all — is the same case.
        assert!(matches!(
            evaluate_worktree_grant("Agent", Some(&serde_json::json!({})), dir.path()),
            Some(WorktreeGrant::Rewrite(_))
        ));
        assert!(matches!(
            evaluate_worktree_grant("Agent", None, dir.path()),
            Some(WorktreeGrant::Rewrite(_))
        ));
    }

    #[test]
    fn does_not_grant_a_read_only_agent() {
        // A positively-identified reader writes nothing, so a worktree would be
        // pure cost — and #3455 is an open complaint about exactly that cost.
        let dir = main_checkout();
        for agent in ["research", "code-critic", "code-analyzer", "ticketing"] {
            assert_eq!(
                evaluate_worktree_grant("Agent", Some(&input(agent, None)), dir.path()),
                None,
                "{agent} only reads and must not be moved"
            );
        }
    }

    #[test]
    fn does_not_grant_an_already_isolated_dispatch() {
        // Both modes separate the tree; neither may be overwritten, least of
        // all `remote`, which the guard has no business downgrading.
        let dir = main_checkout();
        for mode in ["worktree", "remote"] {
            assert_eq!(
                evaluate_worktree_grant(
                    "Agent",
                    Some(&input("rust-engineer", Some(mode))),
                    dir.path()
                ),
                None,
                "isolation={mode} is already a tree of its own"
            );
        }
    }

    #[test]
    fn does_not_grant_outside_a_main_checkout() {
        // Delegated work in a worktree is where work is supposed to happen and
        // must be completely untouched by this rule. A non-repository directory
        // is not a checkout at all.
        let worktree = tempfile::tempdir().expect("tempdir");
        std::fs::write(worktree.path().join(".git"), "gitdir: /elsewhere").expect("write .git");
        assert_eq!(
            evaluate_worktree_grant(
                "Agent",
                Some(&input("rust-engineer", None)),
                worktree.path()
            ),
            None
        );

        let plain = tempfile::tempdir().expect("tempdir");
        assert_eq!(
            evaluate_worktree_grant("Agent", Some(&input("rust-engineer", None)), plain.path()),
            None
        );
    }

    #[test]
    fn does_not_touch_a_non_dispatch_tool() {
        let dir = main_checkout();
        for tool in ["Read", "Edit", "Write", "Bash", "SendMessage"] {
            assert_eq!(
                evaluate_worktree_grant(tool, Some(&input("rust-engineer", None)), dir.path()),
                None,
                "{tool} is not a dispatch"
            );
        }
    }

    #[test]
    fn task_dispatch_is_denied_rather_than_rewritten() {
        // `Task` has no `isolation` parameter, so a rewrite would produce a
        // failed tool call rather than an isolated agent.
        let dir = main_checkout();
        let grant =
            evaluate_worktree_grant("Task", Some(&input("rust-engineer", None)), dir.path())
                .expect("a Task writer in a main checkout must be handled");
        assert_eq!(grant, WorktreeGrant::Deny(TASK_DENY_REASON));
        assert!(TASK_DENY_REASON.contains("Agent"));
        assert!(TASK_DENY_REASON.contains(r#"isolation: "worktree""#));
    }

    #[test]
    fn rewrite_preserves_every_field_the_caller_sent() {
        // `updatedInput` replaces the arguments outright — a dropped prompt
        // would dispatch an agent with no brief.
        let original = serde_json::json!({
            "subagent_type": "rust-engineer",
            "prompt": "do the thing",
            "description": "short label",
            "model": "sonnet",
        });
        let updated = with_granted_isolation(Some(&original));
        assert_eq!(updated["prompt"], "do the thing");
        assert_eq!(updated["description"], "short label");
        assert_eq!(updated["model"], "sonnet");
        assert_eq!(updated["subagent_type"], "rust-engineer");
        assert_eq!(updated["isolation"], "worktree");
    }

    /// A project directory that is a main checkout AND carries a project config.
    ///
    /// Why: the opt-out is only reachable past `is_main_checkout`, so every
    /// #5814 case needs both the `.git` directory and the config file.
    fn main_checkout_with_config(body: &str) -> TempDir {
        let dir = main_checkout();
        std::fs::write(
            dir.path()
                .join(trusty_mpm::core::project_config::PROJECT_CONFIG_FILE),
            body,
        )
        .expect("write project config");
        dir
    }

    #[test]
    fn grants_nothing_when_the_project_opts_out() {
        // #5814: `agent_worktree = false` — the writer stays in the checkout,
        // and the rewrite carries no `isolation` at all.
        let dir = main_checkout_with_config("agent_worktree = false\n");
        let sent = serde_json::json!({"subagent_type": "rust-engineer", "prompt": "do the thing"});
        let grant = evaluate_worktree_grant("Agent", Some(&sent), dir.path())
            .expect("an opted-out project must still annotate the dispatch");
        let WorktreeGrant::InPlace(updated) = grant else {
            panic!("an opted-out project must not be granted a worktree");
        };
        assert!(
            updated.get("isolation").is_none(),
            "the opt-out must not add isolation: {updated}"
        );
        assert_eq!(updated["subagent_type"], "rust-engineer");
    }

    #[test]
    fn opt_out_covers_task_which_would_otherwise_be_denied() {
        // A `Task` writer is denied today only because it needs a worktree it
        // cannot be given. A project that needs no worktree has no such dispatch
        // to refuse.
        let dir = main_checkout_with_config("agent_worktree = false\n");
        let sent = serde_json::json!({"subagent_type": "rust-engineer", "prompt": "go"});
        assert!(
            matches!(
                evaluate_worktree_grant("Task", Some(&sent), dir.path()),
                Some(WorktreeGrant::InPlace(_))
            ),
            "the opt-out must reach Task before the deny"
        );
    }

    #[test]
    fn grants_a_worktree_when_the_project_says_nothing_or_says_yes() {
        // The default branch, which is every project that predates #5814: an
        // absent key, an absent file, an unrelated key, and a config that cannot
        // be parsed at all must ALL leave ADR-0048's grant exactly as it was.
        for body in [
            "default_model = \"opus\"\n",
            "agent_worktree = true\n",
            "worktree = false\n",
            "agent_worktre = false\n",
            "agent_worktree = \"false\"\n",
            "agent_worktree = false\nnot toml at all(",
        ] {
            let dir = main_checkout_with_config(body);
            let sent = serde_json::json!({"subagent_type": "rust-engineer", "prompt": "go"});
            let grant = evaluate_worktree_grant("Agent", Some(&sent), dir.path())
                .expect("a writer in a main checkout is still granted a worktree");
            let WorktreeGrant::Rewrite(updated) = grant else {
                panic!("expected the ordinary grant for config: {body}");
            };
            assert_eq!(updated["isolation"], "worktree", "config was: {body}");
        }
    }

    #[test]
    fn in_place_notice_is_appended_to_the_prompt() {
        // The note is how the agent learns the workflow that replaces isolation;
        // the original brief must survive in front of it.
        let sent =
            serde_json::json!({"subagent_type": "documentation", "prompt": "write the memo"});
        let updated = with_in_place_notice(Some(&sent)).expect("a prompted dispatch is annotated");
        let prompt = updated["prompt"].as_str().expect("prompt stays a string");
        assert!(prompt.starts_with("write the memo"));
        assert!(prompt.ends_with(IN_PLACE_WORKFLOW_NOTE));
        assert!(prompt.contains("agent_worktree = false"));
        assert_eq!(updated["subagent_type"], "documentation");
    }

    #[test]
    fn in_place_notice_is_not_appended_twice() {
        let sent = serde_json::json!({"prompt": format!("brief{IN_PLACE_WORKFLOW_NOTE}")});
        assert_eq!(with_in_place_notice(Some(&sent)), None);
    }

    #[test]
    fn in_place_leaves_a_promptless_dispatch_alone() {
        // Nothing to annotate, and inventing a `prompt` the caller never sent
        // would rewrite the dispatch into one it did not make.
        assert_eq!(with_in_place_notice(None), None);
        assert_eq!(
            with_in_place_notice(Some(&serde_json::json!({"subagent_type": "documentation"}))),
            None
        );
        assert_eq!(
            with_in_place_notice(Some(&serde_json::json!({"prompt": 7}))),
            None
        );
    }

    #[test]
    fn grant_response_has_the_documented_shape() {
        let body = build_worktree_grant_response(&serde_json::json!({"isolation": "worktree"}));
        let parsed: Value = serde_json::from_str(&body).expect("valid JSON");
        assert_eq!(parsed["hookSpecificOutput"]["hookEventName"], "PreToolUse");
        assert_eq!(
            parsed["hookSpecificOutput"]["updatedInput"]["isolation"],
            "worktree"
        );
        // A rewrite must never carry a permission decision: it changes the
        // arguments and leaves the permission flow alone.
        assert!(
            parsed["hookSpecificOutput"]
                .get("permissionDecision")
                .is_none()
        );
    }
}
