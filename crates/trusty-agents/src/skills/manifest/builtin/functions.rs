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
//! path (authored `.md`, MCP discovery) that can author one. That closure is
//! also why `cto-tools` below ships as reviewed `const` data rather than as a
//! user-authoring mechanism: the owner asked for it as a "user-created" bundle,
//! and the decision (owner, 2026-07-28) is to ship the row now and treat
//! operator-authored bundles as a separate follow-on (#2201), because the
//! authoring path is the part that needs the security design, not the row.
//! What: A `const` table of [`function_skill`] rows, each naming member skill
//! ids that exist elsewhere in [`super::all`].
//!
//! **ADR-0024 decision 4 (owner, 2026-07-29):** the `ticketing` row was REMOVED
//! from this table, together with the twelve leaf ticket/CI skills it bundled
//! (`super::ops`). The owner's ruling is that ticketing is reachable as a
//! SUB-AGENT only — never as a skill — so the grouped card and its members are
//! gone from the skills surface entirely. Nothing about the ticketing TOOLS
//! changed: `create_ticket` and friends are still registered and still granted
//! to `ticketing-agent.toml`, which is the agent that owns the domain. The two
//! authored `.trusty-agents/skills/ticketing-*.md` assets are a DIFFERENT
//! subsystem — that sub-agent's own domain knowledge, injected into its system
//! prompt — and are untouched.
//! Test: `super::super::tests` — `ticketing_is_absent_from_the_skill_surface`,
//! `google_workspace_function_skill_bundles_the_mail_and_tasks_leaves`,
//! `cto_tools_function_skill_bundles_the_four_cto_database_leaves`,
//! `function_skill_never_leaks_its_bundle_id_into_the_tool_patterns`.

use super::super::{SkillDef, function_skill};

/// The Google Workspace mail-and-tasks work unit.
///
/// Why: The owner's screen showed `create_draft`, `list_tasks`, `complete_task`
/// and `create_task` as four unrelated cards; they are one function — "handle my
/// mail and my to-do list". This row is that function, authored exactly as
/// #4023 authored `ticketing`: an explicit, closed, reviewable list.
///
/// **What is deliberately NOT in it**, since a bundle's value is that a reviewer
/// need only read one line, which only works if the line does not hide teeth:
/// - `gmail-filters` / `gmail-settings` — *account* configuration (vacation
///   responder, forwarding). A forwarding rule is a mail-exfiltration and
///   account-persistence primitive; it does not belong in a grant a reviewer
///   reads as "can send mail and tick off tasks".
/// - `gw-account-*` — OAuth account administration (authorize, remove, set
///   default). Administering *which* Google identity every other gworkspace tool
///   acts as is a different function from using it.
/// - Drive, Docs, Sheets, Slides and Calendar — the other Workspace families.
///   They are not "mail and tasks", and the description says so rather than
///   letting the bundle's name imply a coverage it does not have.
///
/// **Provider verification across divergent members** (S-16). Every member here
/// carries the same manifest-level credential ([`super::GOOGLE_OAUTH`], whose
/// `env_var` is `None`), so the group rolls up to exactly one requirement,
/// reported as NOT VERIFIED — never as configured. The rollup is a *set of
/// distinct requirements*, not a single collapsed verdict: a bundle whose members
/// carried two different providers reports both, so a divergence is visible
/// rather than averaged away (see `api::server::agent_skills::function_groups`).
/// The dotted-scope axis that DOES distinguish these leaves at dispatch time
/// (`google.gmail.*` vs `google.tasks.*`) is not a manifest concept and is not
/// claimed here; it is surfaced separately and honestly by the route's
/// `dead_scope_patterns`.
///
/// **On the display name — read this before trusting the card.** The name is the
/// short "Google Workspace" (owner decision, 2026-07-28, made with the narrower
/// alternative and this caveat in front of him). The MEMBERSHIP is narrower than
/// that name: Gmail and Google Tasks ONLY. This row holds no Drive, Docs, Sheets,
/// Slides or Calendar skill, and none of the exclusions listed above. Read the
/// member list, not the name, when reviewing what a `google-workspace` grant
/// confers; the card's description states the same scope to the user.
/// Test: `google_workspace_function_skill_bundles_the_mail_and_tasks_leaves`,
/// `google_workspace_bundle_excludes_account_and_settings_administration`,
/// `super::super::super::api::server::tests::agent_skills::skills_route_rolls_up_group_provider_requirements`.
const GOOGLE_WORKSPACE_MEMBERS: &[&str] = &[
    // --- mail -------------------------------------------------------------
    "gmail-search",
    "gmail-read",
    "gmail-compose",
    "gmail-draft",
    "gmail-format",
    "gmail-labels-apply",
    "gmail-labels-manage",
    "gmail-attachments-list",
    "gmail-attachment-download",
    // --- tasks ------------------------------------------------------------
    "gtasks-lists",
    "gtasks-manage",
    "gtasks-list",
    "gtasks-complete",
    "gtasks-create",
];

/// The four CTO operations-database queries, as one function.
///
/// Why (owner, 2026-07-28): the CTO assistant's four `query_*` cards are one
/// capability — "ask the CTO operations database" — and the owner asked to see
/// them as one row. He framed it as a *user-created* bundle; the decision this
/// session was to ship it as reviewed `const` data and keep operator authoring as
/// a follow-on (#2201). That is not a shortcut: S-14 makes bundles built-in only
/// *because* a bundle id is the one name whose meaning a reviewer cannot check by
/// reading `agent.toml`, so adding an authoring path is a security design, not a
/// parser change. Shipping the row now gives the owner the grouping he asked for
/// without relaxing that rule by a single line.
/// Test: `cto_tools_function_skill_bundles_the_four_cto_database_leaves`.
const CTO_TOOLS_MEMBERS: &[&str] = &[
    "cto-headcount",
    "cto-budget",
    "cto-risks",
    "cto-work-classification",
];

pub(super) static TABLE: &[SkillDef] = &[
    function_skill(
        "google-workspace",
        // Short name by owner decision; membership is Gmail + Tasks only — see
        // the `GOOGLE_WORKSPACE_MEMBERS` doc comment.
        "Google Workspace",
        "Gmail and Google Tasks as one capability: search, read, compose, draft, \
         label and download attachments from mail, and list, create, update and \
         complete tasks. Covers mail and tasks ONLY — Drive, Docs, Sheets, Slides \
         and Calendar are separate skills, and Gmail account settings, filters and \
         Google account authorization are deliberately excluded. Every member needs \
         an authorized Google account, which this endpoint cannot verify.",
        GOOGLE_WORKSPACE_MEMBERS,
    ),
    function_skill(
        "cto-tools",
        "CTO Tools",
        "Read the CTO operations database as one capability: engineering headcount, \
         budget, the risk register and work classification. Read-only queries; the \
         database is supplied by a per-persona plugin, so an agent granted this on a \
         machine without it holds four skills that resolve to nothing.",
        CTO_TOOLS_MEMBERS,
    ),
];
