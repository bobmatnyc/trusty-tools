//! Function skills — the bundling tier over the 1:1 leaf catalog (#4022/#4023).
//!
//! Why: The owner's directive (2026-07-26) — *"Skills also need to be organized
//! by function. So 'ticketing' is a skill with various capabilities including
//! all the skills under one."* — sits one tier ABOVE the one-skill-per-tool
//! model, not in place of it. Epic #4021's OQ-1 was resolved *grant-the-bundle*,
//! so a row here is a real grant: naming it in `[skills].allow` grants every
//! member. Nothing about the leaf tier changes — each of these members is still
//! independently grantable, still wraps exactly one tool, and the bundle
//! compiles down to those leaves before any permission gate runs.
//!
//! **Why bundles are `const` and built-in only.** A bundle is the one skill
//! shape that turns one line of config into N capabilities, which is exactly the
//! shape a privilege-escalation bug wants. Keeping the member lists in
//! compile-time data means the set is closed, reviewable in a diff, and checked
//! by `builtin_function_skills_declare_only_known_members` — there is no runtime
//! path (authored `.md`, MCP discovery) that can author one.
//! What: A `const` table of [`function_skill`] rows, each naming member skill
//! ids that exist elsewhere in [`super::all`].
//! Test: `super::super::tests` — `ticketing_function_skill_bundles_the_ten_ticket_leaf_skills`,
//! `function_skill_never_leaks_its_bundle_id_into_the_tool_patterns`.

use super::super::{SkillDef, function_skill};

/// The ten ticketing leaf skills, in `ops.rs` catalog order.
///
/// Why: Spelled out rather than derived from a name prefix — DOC-57 §5.5/S-8 is
/// explicit that *"no prefix grouping [exists] anywhere in this model"*, and a
/// `ticket-*` prefix scan would be precisely that: a glob dialect reintroduced
/// through the back door, silently absorbing any future `ticket-`-prefixed skill
/// into an existing grant. An explicit list means adding a leaf is a deliberate
/// decision to widen the bundle, visible in review.
/// Test: `ticketing_function_skill_bundles_the_ten_ticket_leaf_skills`.
const TICKETING_MEMBERS: &[&str] = &[
    "ticket-create",
    "ticket-read",
    "ticket-update",
    "ticket-close",
    "ticket-list",
    "ticket-comment",
    "ticket-tag",
    "ticket-assign",
    "ticket-transition",
    "ticket-search",
];

pub(super) static TABLE: &[SkillDef] = &[function_skill(
    "ticketing",
    "Ticketing",
    "Everything needed to work a tracker end to end: open, read, update, comment \
     on, label, assign, transition, search and close tickets. Granting this one \
     skill grants all ten ticket operations. Note: per the domain authority \
     (epic #4021 section D), the ticketing DOMAIN routes to the subagent \
     modality when a ticketing subagent is available — this function skill is \
     the direct-skill fallback used when it is not.",
    TICKETING_MEMBERS,
)];
