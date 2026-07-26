//! Tool-less skills — guidance with no executable member.
//!
//! Why: The owner's model admits these explicitly — *"There can be other skills
//! without tools"* — so they are modelled as a first-class shape, not as a
//! degenerate case of a tool-wrapping skill. A tool-less skill is how an agent
//! is taught a *procedure* (how to run a release, how to hand work off) as
//! opposed to being handed a *capability*.
//!
//! These rows are deliberately few. The natural home for procedural guidance is
//! an authored `.md` in a skill source — that file carries the actual prose,
//! which a `const &'static str` cannot. What lives here is the small set that
//! the resolver needs to be able to name before any source is scanned, so that
//! `[skills].allow` can reference them on a machine with no skill files at all.
//! An authored manifest of the same id supersedes any row here (`with_authored`).
//! What: A `const` table of zero-tool [`SkillDef`] rows.
//! Test: `super::super::tests::tool_less_skills_are_present_and_expand_to_no_tools`.

use super::super::{SkillDef, SkillKind::Knowledge, SkillKind::System, prose_skill};

pub(super) static TABLE: &[SkillDef] = &[
    prose_skill(
        "handoff-protocol",
        "Work Handoff",
        "How to hand work to another agent: what was done, what remains, what constrains it.",
        System,
    ),
    prose_skill(
        "verification-discipline",
        "Verification Before Completion",
        "How to prove work is finished — run it, show the raw output, never assert a result you did not observe.",
        System,
    ),
    prose_skill(
        "owner-context",
        "Owner Context",
        "Who the user is, how they prefer to be addressed, and what they expect from this agent.",
        Knowledge,
    ),
];
