//! Working-tree isolation policy for native Agent-tool dispatches (#4480).
//!
//! Why: an in-conversation `Agent`/`Task` subagent inherits the dispatching
//! PM's working directory. Two concurrent file-mutating subagents in that one
//! directory fight over a single git HEAD, and git does not stop them: a
//! `git checkout -b` refuses only when a *tracked* file differs between both
//! branches' committed versions AND carries an uncommitted local change.
//! Untracked new files, and edits to files not tracked-differently between the
//! branches, transfer onto the other agent's branch silently — which is exactly
//! the shape two mostly-disjoint feature branches have. The observed failure is
//! `git status` reporting the PM on agent A's branch with agent B's changes
//! staged alongside it, with no error at any step.
//!
//! What: the pure classifiers both halves of the guard share — the daemon, which
//! records what each dispatch declared, and `tm hook --pm-guard`, which decides.
//! Two questions, deliberately separate:
//!
//! * [`isolation_separates_working_tree`] — did this dispatch ask for its own
//!   tree? `isolation: "worktree"` (a private git worktree) and
//!   `isolation: "remote"` (a different machine entirely) both do; nothing else
//!   does, including the absent field, which is the default and the whole
//!   problem.
//! * [`agent_mutates_files`] — is the named agent one that writes to its cwd?
//!
//! **Both classifiers fail toward ALLOW, and that direction is not negotiable.**
//! A false DENY lands on the PM and halts every dispatch in the system; a false
//! ALLOW merely reproduces the behaviour that shipped before this module. So
//! [`agent_mutates_files`] answers `true` only for an agent this binary ships
//! and can positively identify as file-mutating — an unknown name, a custom
//! project agent, a renamed agent, or an unparseable frontmatter all answer
//! `false` and the dispatch proceeds.
//!
//! What the classifier covers, and why not more: an agent counts when its own
//! bundled definition tells it to write to the tree it is dispatched into.
//! #4480 shipped engineer-tier alone on the theory that the adjacent tiers need
//! to see the engineer's work in the shared tree; #5650 found that reasoning
//! backwards for three of them. `documentation` creates and reorganises files,
//! `git mv`s them, and commits; `version-control` branches, commits, and pushes;
//! `qa` writes test files. Wanting to read the engineer's work does not make
//! writing alongside it safe — a `documentation` agent dispatched next to a
//! `rust-engineer`, both unisolated, is the same single-HEAD collision the module
//! exists to stop, and it passed with no deny at all until #5650.
//!
//! A dispatch that only reads — research, code analysis, adversarial review,
//! ticketing — still answers `false` and still parallelises freely. That is where
//! the line sits now: writes, not proximity to writes.
//!
//! **The fail-open direction is scoped to the shared-tree race, not to the
//! main checkout (ADR-0048).** Everything above is #4480's calculus, and it
//! still governs [`agent_mutates_files`] and [`shares_the_callers_tree`]. It
//! does NOT govern a session standing in a project's MAIN CHECKOUT, where
//! ADR-0044 forbids source writes outright: there an unknown agent is the exact
//! reported harm — a custom or renamed agent answers `false` here and would
//! keep writing to the tree three sessions share. So
//! [`requires_own_worktree_in_main_checkout`] reads the same bundle through
//! [`agent_write_risk`] and treats `Unknown` as a writer. The two directions do
//! not conflict because the costs are not symmetric between them: in a worktree
//! a false deny halts dispatch and a false allow costs nothing new, while in a
//! main checkout a false allow corrupts another session's branch and a false
//! deny costs one worktree.
//!
//! Test: the `#[cfg(test)]` suite below.
//!
//! [`isolation_separates_working_tree`]: crate::core::dispatch_isolation::isolation_separates_working_tree
//! [`agent_mutates_files`]: crate::core::dispatch_isolation::agent_mutates_files
//! [`shares_the_callers_tree`]: crate::core::dispatch_isolation::shares_the_callers_tree
//! [`agent_write_risk`]: crate::core::dispatch_isolation::agent_write_risk
//! [`requires_own_worktree_in_main_checkout`]: crate::core::dispatch_isolation::requires_own_worktree_in_main_checkout

use serde_json::Value;
use trusty_agents_common::agents::metadata::agent_metadata_from_str;

/// `isolation` values that give a dispatched subagent a working tree of its own.
///
/// Why: the guard's entire question is "does this subagent get its own tree?",
/// and only the harness can answer it. These are the two `isolation` modes the
/// Agent tool documents as producing one — a private git worktree, and a remote
/// cloud environment. An unrecognised value is treated as no isolation rather
/// than assumed safe, because a typo (`"worktrees"`, `"Worktree"`) must not
/// silently buy an exemption.
/// What: exact, case-sensitive values matched by
/// [`isolation_separates_working_tree`]; the harness emits the literal it was
/// given, so an exact match is correct.
/// Test: `isolation_values_that_separate_the_tree`,
/// `unrecognised_isolation_does_not_separate_the_tree`.
pub const ISOLATING_DISPATCH_MODES: &[&str] = &["worktree", "remote"];

/// Frontmatter `role:` values whose agents mutate files in their cwd.
///
/// Why: `role:` is the declared domain every bundled agent carries, so this is a
/// property of the agent definition rather than a name-shaped guess that a
/// rename would silently invalidate. A role belongs here only when EVERY bundled
/// agent declaring it writes files; a role with a read-only member is keyed by
/// name in [`FILE_MUTATING_NAMES`] instead.
/// What: matched case-sensitively against [`crate::core::agent_metadata::AgentMetadata::role`].
/// `data-engineer` is listed explicitly because it declares that role rather
/// than plain `engineer` while still extending `base-engineer`. `documentation`
/// and `version-control` each have exactly one bundled agent, and both write —
/// docs and `git mv`s for one, branches and commits for the other.
/// Test: `engineer_tier_agents_mutate_files`, `file_writing_non_engineer_agents_mutate_files`,
/// `non_engineer_agents_do_not`.
// #5650: documentation and version-control demonstrably write files; omitting
// them let two unisolated writers share one HEAD with no deny.
const FILE_MUTATING_ROLES: &[&str] = &[
    "engineer",
    "data-engineer",
    "documentation",
    "version-control",
];

/// Bundled agent `name:`s that mutate files but whose `role:` cannot say so.
///
/// Why: this list is keyed on NAME rather than role, and that is deliberate —
/// do not fold it back into [`FILE_MUTATING_ROLES`]. All four bundled agents
/// declaring `role: qa` also declare `extends: base-qa`, but they do not agree
/// about writing: `qa`, `web-qa`, and `api-qa` author test files, while
/// `code-critic` is a pure reviewer that reads code and returns a verdict
/// (`code-critic.md` — "You did not write this code"). Adding `qa` to the role
/// list would classify `code-critic` as mutating and deny a review dispatched
/// alongside an engineer, which is the ordinary workflow.
/// What: matched case-sensitively against the agent's declared `name:`.
/// Test: `file_writing_non_engineer_agents_mutate_files`,
/// `code_critic_is_not_file_mutating`.
// #5650: role: qa is not homogeneous — code-critic shares it and only reviews.
const FILE_MUTATING_NAMES: &[&str] = &["qa", "web-qa", "api-qa"];

/// The `extends:` base whose descendants are engineer-tier regardless of role.
///
/// Why: a bundled agent could declare a new role spelling and still inherit the
/// whole engineer contract. Checking the inheritance edge as well as the role
/// means adding such an agent does not silently drop it out of the guard.
/// What: matched case-sensitively against
/// [`crate::core::agent_metadata::AgentMetadata::extends`].
/// Test: `engineer_tier_agents_mutate_files`.
const FILE_MUTATING_BASE: &str = "base-engineer";

/// Does this dispatch's `isolation` declaration give it its own working tree?
///
/// Why: this is the one thing that makes a concurrent file-mutating dispatch
/// safe, so it is also the one thing the guard accepts as an exemption.
/// What: `true` when `isolation` is present and is a member of
/// [`ISOLATING_DISPATCH_MODES`]. `None` — the default, and the case the whole
/// issue is about — is `false`.
/// Test: `isolation_values_that_separate_the_tree`,
/// `unrecognised_isolation_does_not_separate_the_tree`.
pub fn isolation_separates_working_tree(isolation: Option<&str>) -> bool {
    isolation.is_some_and(|v| ISOLATING_DISPATCH_MODES.contains(&v))
}

/// Is `agent` a bundled agent that mutates files in its working directory?
///
/// Why: the guard must not fire on a read-only research, review, or planning
/// dispatch — those share a cwd harmlessly and are dispatched in parallel all
/// the time. Resolving the answer from the agent's own declared frontmatter
/// keeps one authority for it instead of a name list that drifts as agents are
/// added.
/// What: scans the compiled-in bundle for the agent whose declared `name:`
/// equals `agent`, and reports whether its `role:` is in
/// [`FILE_MUTATING_ROLES`], its `extends:` is [`FILE_MUTATING_BASE`], or its
/// name is in [`FILE_MUTATING_NAMES`]. A name this binary does not ship — a
/// project-local or custom agent — answers `false`, the fail-open direction
/// this module's doc commits to.
///
/// No caching and no I/O: the bundle is a compile-time table of ~40 entries and
/// this runs once per `Agent` dispatch, which is rare compared to ordinary tool
/// calls. A process-lifetime cache would be global state for no measurable win.
/// Test: `engineer_tier_agents_mutate_files`,
/// `file_writing_non_engineer_agents_mutate_files`, `non_engineer_agents_do_not`,
/// `unknown_agent_fails_open`.
pub fn agent_mutates_files(agent: &str) -> bool {
    agent_write_risk(agent) == AgentWriteRisk::Writes
}

/// What this binary can say about whether `agent` writes to its cwd.
///
/// Why: [`agent_mutates_files`] collapses two different answers into `false` —
/// "this bundled agent reads only" and "this binary has never heard of this
/// agent". #4480 was right to treat both as ALLOW, because its question was
/// whether to halt a dispatch over a race. ADR-0044's question is different:
/// may this agent write to a checkout other sessions are standing in? There,
/// the two answers must diverge, so the classifier has to keep them apart
/// before anything collapses them.
/// What: `Writes` when the bundled definition says it writes, `ReadsOnly` when
/// the bundled definition says it does not, `Unknown` for an empty name and for
/// any name this binary does not ship — a custom project agent, a renamed
/// agent, or an unparseable definition.
/// Test: `write_risk_separates_unknown_from_read_only`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentWriteRisk {
    /// The bundled definition declares an agent that writes files.
    Writes,
    /// The bundled definition declares an agent that only reads.
    ReadsOnly,
    /// Not a bundled agent this binary can classify at all.
    Unknown,
}

/// Classify `agent` against the compiled-in bundle.
///
/// Why: one bundle scan feeding both policies, so the shared-tree race and the
/// main-checkout boundary can never disagree about what an agent is.
/// What: scans the compiled-in bundle for the agent whose declared `name:`
/// equals `agent` and reports whether its `role:` is in [`FILE_MUTATING_ROLES`],
/// its `extends:` is [`FILE_MUTATING_BASE`], or its name is in
/// [`FILE_MUTATING_NAMES`]. A name not in the bundle is
/// [`AgentWriteRisk::Unknown`], never `ReadsOnly` — the distinction the whole
/// enum exists for.
///
/// No caching and no I/O: the bundle is a compile-time table of ~40 entries and
/// this runs once per `Agent` dispatch, which is rare compared to ordinary tool
/// calls. A process-lifetime cache would be global state for no measurable win.
/// Test: `write_risk_separates_unknown_from_read_only`,
/// `engineer_tier_agents_mutate_files`, `non_engineer_agents_do_not`.
pub fn agent_write_risk(agent: &str) -> AgentWriteRisk {
    if agent.is_empty() {
        return AgentWriteRisk::Unknown;
    }
    crate::core::bundle::ALL
        .iter()
        .filter(|artifact| {
            artifact
                .rel_path
                .strip_prefix("agents/")
                .is_some_and(|f| f.ends_with(".md"))
        })
        .map(|artifact| agent_metadata_from_str(artifact.contents))
        .find(|meta| meta.name.as_deref() == Some(agent))
        .map_or(AgentWriteRisk::Unknown, |meta| {
            let writes = meta
                .role
                .as_deref()
                .is_some_and(|r| FILE_MUTATING_ROLES.contains(&r))
                || meta.extends.as_deref() == Some(FILE_MUTATING_BASE)
                // #5650: name-keyed because `role: qa` mixes writers with the
                // read-only `code-critic`; see FILE_MUTATING_NAMES.
                || FILE_MUTATING_NAMES.contains(&agent);
            if writes {
                AgentWriteRisk::Writes
            } else {
                AgentWriteRisk::ReadsOnly
            }
        })
}

/// Must this dispatch get a working tree of its own before it may run in a
/// project's main checkout (ADR-0048)?
///
/// Why: ADR-0044 makes the main checkout read-only except for documents and
/// configuration, for the PM and for every agent it dispatches. An agent that
/// might write therefore needs somewhere else to write, and the only somewhere
/// else is its own worktree. The predicate is separate from
/// [`shares_the_callers_tree`] because it answers a different question and
/// resolves the indeterminate case the other way: this one has no concurrent
/// sibling in it at all, so the FIRST unisolated writer is already a violation.
/// What: `true` when the declared isolation does not
/// [`isolation_separates_working_tree`] AND [`agent_write_risk`] is anything
/// other than [`AgentWriteRisk::ReadsOnly`]. `Unknown` is included
/// deliberately — see the module doc for why the fail-open direction does not
/// carry across this boundary.
/// Test: `main_checkout_isolation_is_required_for_writers_and_unknowns`.
pub fn requires_own_worktree_in_main_checkout(agent: &str, isolation: Option<&str>) -> bool {
    !isolation_separates_working_tree(isolation)
        && agent_write_risk(agent) != AgentWriteRisk::ReadsOnly
}

/// The `subagent_type` an Agent-tool `tool_input` names, when it names one.
///
/// Why: one extraction shared by the daemon's tracker and the guard, so the two
/// can never read different fields and disagree about which agent was
/// dispatched.
/// What: `tool_input.subagent_type` as a non-empty `&str`. An untyped
/// dispatch — no `subagent_type` at all — yields `None` and therefore fails
/// open; it is a separate defect, not this guard's to block.
/// Test: `reads_subagent_type_and_isolation`.
pub fn dispatch_agent(tool_input: Option<&Value>) -> Option<&str> {
    dispatch_field(tool_input, "subagent_type")
}

/// The `isolation` mode an Agent-tool `tool_input` declares, when it declares one.
///
/// Why: as [`dispatch_agent`] — one reader for the field the whole policy turns on.
/// What: `tool_input.isolation` as a non-empty `&str`.
/// Test: `reads_subagent_type_and_isolation`.
pub fn dispatch_isolation(tool_input: Option<&Value>) -> Option<&str> {
    dispatch_field(tool_input, "isolation")
}

/// Read one non-empty string field from an Agent-tool `tool_input`.
fn dispatch_field<'a>(tool_input: Option<&'a Value>, key: &str) -> Option<&'a str> {
    tool_input
        .and_then(|i| i.get(key))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

/// Would this dispatch put a second file-mutating agent into the caller's tree?
///
/// Why: the composed question every caller actually asks, kept here so the
/// daemon (recording) and the guard (deciding) cannot compose the two
/// classifiers differently.
/// What: `true` when the named agent [`agent_mutates_files`] AND the declared
/// isolation does not [`isolation_separates_working_tree`].
/// Test: `shares_the_callers_tree_only_for_unisolated_engineers`.
pub fn shares_the_callers_tree(agent: &str, isolation: Option<&str>) -> bool {
    agent_mutates_files(agent) && !isolation_separates_working_tree(isolation)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn isolation_values_that_separate_the_tree() {
        for mode in ISOLATING_DISPATCH_MODES {
            assert!(
                isolation_separates_working_tree(Some(mode)),
                "{mode} must count as its own working tree"
            );
        }
    }

    #[test]
    fn unrecognised_isolation_does_not_separate_the_tree() {
        // The absent field is the default and the entire defect; a typo or a
        // future mode this binary does not know must not buy an exemption.
        for v in [
            None,
            Some(""),
            Some("Worktree"),
            Some("worktrees"),
            Some("no"),
        ] {
            assert!(
                !isolation_separates_working_tree(v),
                "{v:?} must not count as its own working tree"
            );
        }
    }

    #[test]
    fn engineer_tier_agents_mutate_files() {
        // Named agents that ship in this binary and declare the engineer tier
        // either by `role:` or by extending `base-engineer`.
        for agent in [
            "rust-engineer",
            "engineer",
            "python-engineer",
            "refactoring-engineer",
            "data-engineer",
        ] {
            assert!(
                agent_mutates_files(agent),
                "{agent} is engineer-tier and must classify as file-mutating"
            );
        }
    }

    #[test]
    fn file_writing_non_engineer_agents_mutate_files() {
        // #5650: these five write files and got no isolation at all before it.
        // `documentation` and `version-control` arrive by role; the three QA
        // agents by name, because `role: qa` also covers `code-critic`.
        for agent in ["documentation", "version-control", "qa", "web-qa", "api-qa"] {
            assert!(
                agent_mutates_files(agent),
                "{agent} writes files and must classify as file-mutating"
            );
        }
    }

    #[test]
    fn code_critic_is_not_file_mutating() {
        // The trap FILE_MUTATING_NAMES exists to avoid. `code-critic` declares
        // the same `role: qa` and `extends: base-qa` as the three QA writers,
        // but it only reads code and returns a verdict. Widening by role would
        // deny a review dispatched alongside the engineer it reviews.
        assert!(
            !agent_mutates_files("code-critic"),
            "code-critic reviews and never writes; a role-based widen would misclassify it"
        );
    }

    #[test]
    fn non_engineer_agents_do_not() {
        // The read-only tiers the PM dispatches in parallel routinely. Denying
        // these would halt the ordinary workflow to prevent a race they are not
        // the trigger for — see the module doc.
        for agent in [
            "research",
            "code-critic",
            "code-analyzer",
            "security",
            "ticketing",
        ] {
            assert!(
                !agent_mutates_files(agent),
                "{agent} must not classify as file-mutating"
            );
        }
    }

    #[test]
    fn unknown_agent_fails_open() {
        // A custom/project agent, a rename, or an empty name is INDETERMINATE,
        // and indeterminate must resolve to "does not mutate" — the fail-open
        // direction. A false deny here lands on the PM and stops all dispatch.
        for agent in ["", "some-project-custom-agent", "Rust-Engineer", "unknown"] {
            assert!(
                !agent_mutates_files(agent),
                "{agent:?} is not a bundled engineer-tier agent and must fail open"
            );
        }
    }

    #[test]
    fn write_risk_separates_unknown_from_read_only() {
        // The distinction `agent_mutates_files` collapses. `code-critic` is a
        // bundled agent this binary positively knows does not write; the other
        // three are names it has never seen. Both answer `false` there, and
        // ADR-0048 needs them apart.
        assert_eq!(agent_write_risk("rust-engineer"), AgentWriteRisk::Writes);
        assert_eq!(agent_write_risk("code-critic"), AgentWriteRisk::ReadsOnly);
        assert_eq!(agent_write_risk("research"), AgentWriteRisk::ReadsOnly);
        for unknown in ["", "some-project-custom-agent", "Rust-Engineer"] {
            assert_eq!(
                agent_write_risk(unknown),
                AgentWriteRisk::Unknown,
                "{unknown:?} is not a bundled agent"
            );
        }
    }

    #[test]
    fn main_checkout_isolation_is_required_for_writers_and_unknowns() {
        // ADR-0048: in a main checkout the indeterminate case resolves toward
        // isolation, the opposite of `shares_the_callers_tree`. A custom agent
        // is exactly the writer that kept landing in the shared checkout.
        for agent in ["rust-engineer", "documentation", "custom-agent", ""] {
            assert!(
                requires_own_worktree_in_main_checkout(agent, None),
                "{agent:?} must not run unisolated in a main checkout"
            );
        }
        // A positively-identified reader costs nothing to run in place.
        for agent in ["research", "code-critic", "ticketing"] {
            assert!(
                !requires_own_worktree_in_main_checkout(agent, None),
                "{agent} only reads and must not be forced into a worktree"
            );
        }
        // Declared isolation is the whole remedy; it must satisfy the rule.
        for mode in ["worktree", "remote"] {
            assert!(!requires_own_worktree_in_main_checkout(
                "rust-engineer",
                Some(mode)
            ));
            assert!(!requires_own_worktree_in_main_checkout(
                "custom-agent",
                Some(mode)
            ));
        }
    }

    #[test]
    fn unknown_agents_diverge_between_the_two_policies() {
        // The one assertion that pins ADR-0048's actual claim: the same unknown
        // agent is ALLOWED to share a worktree's HEAD (#4480's fail-open) and
        // REQUIRED to be isolated in a main checkout. If a later change makes
        // these agree, one of the two decisions has been silently reversed.
        assert!(!shares_the_callers_tree("custom-agent", None));
        assert!(requires_own_worktree_in_main_checkout("custom-agent", None));
    }

    #[test]
    fn reads_subagent_type_and_isolation() {
        let input = serde_json::json!({
            "subagent_type": "rust-engineer",
            "isolation": "worktree",
            "prompt": "…",
        });
        assert_eq!(dispatch_agent(Some(&input)), Some("rust-engineer"));
        assert_eq!(dispatch_isolation(Some(&input)), Some("worktree"));

        // Absent, empty, and non-string all read as "not declared".
        let bare = serde_json::json!({"subagent_type": "", "isolation": 7});
        assert_eq!(dispatch_agent(Some(&bare)), None);
        assert_eq!(dispatch_isolation(Some(&bare)), None);
        assert_eq!(dispatch_agent(None), None);
        assert_eq!(dispatch_isolation(None), None);
    }

    #[test]
    fn shares_the_callers_tree_only_for_unisolated_engineers() {
        assert!(shares_the_callers_tree("rust-engineer", None));
        assert!(!shares_the_callers_tree("rust-engineer", Some("worktree")));
        assert!(!shares_the_callers_tree("rust-engineer", Some("remote")));
        assert!(!shares_the_callers_tree("research", None));
        assert!(!shares_the_callers_tree("unknown-agent", None));
    }
}
