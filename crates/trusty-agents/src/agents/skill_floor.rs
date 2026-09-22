//! The server-owned SKILL floor for assistant-kind agents (#7881).
//!
//! Why: sub-agent reachability has had a server-owned, narrow-only floor since
//! ADR-0024 decision 4 ([`super::delegation::ASSISTANT_REACHABLE_SUBAGENTS`],
//! enforced at dispatch by `tools::subagent_allow::SubagentAllowSet::resolve`
//! and on the write path by `tools::subagent_allow::narrow_to_floor`). Skills
//! had no analog: a skill reached an assistant by being NAMED in
//! `[system_prompt].skills`, and `crate::tools::skill_loader::FsSkillResolver`
//! resolves a name against `~/.claude/skills/` — the operator's whole Claude
//! Code library. `ctrl_turn` widens that further by BM25-matching the user's
//! own input against every discovered skill and injecting the top three, so a
//! message mentioning "rust" or "tests" could pull a coding skill into an
//! assistant's prompt with nothing in the product deciding it was allowed.
//! Owner ruling (2026-09-14): *"we've overcomplicated permissions. For now, it
//! should have access to use the limited set of subagents and skills, the ones
//! we've bundled with mpm, but not coding ones."*
//!
//! What: [`ASSISTANT_REACHABLE_SKILLS`] is the ceiling — the bundled
//! NON-CODING skill names, derived from trusty-mpm's
//! `assets/framework-manifest.toml` `[skill_categories].universal` list and
//! from `crates/trusty-agents/.trusty-agents/skills/`.
//! [`reachable_skills`] intersects it with the agent's own `[skills].allow`,
//! and [`skill_is_reachable`] is the per-name predicate the three prompt
//! injection sites call. [`narrow_skills_to_floor`] is the write-path
//! counterpart, enforced in `api::server::assistant_settings::patch_grants`.
//!
//! Two differences from the sub-agent floor, both deliberate:
//!   - An ABSENT `[skills].allow` means THE FLOOR, not nothing. A sub-agent
//!     whitelist had to fail closed to empty because delegation spawns a
//!     process; a skill is prompt text, and every shipped persona already
//!     names skills it expects to be injected, so an empty default would be a
//!     silent capability loss on rollout with no seed to migrate.
//!   - The floor binds only the ASSISTANT KIND
//!     ([`super::delegation::is_assistant_kind`]). A coding sub-agent is
//!     supposed to reach coding skills; narrowing it here would be a different,
//!     unratified decision.
//!
//! Fail-closed on every unknown: a blank name, a name off the floor, and an
//! agent whose role cannot be read at all (see
//! [`role_is_assistant_kind_or_unknown`]) all REFUSE. Nothing in this module
//! can answer "everything" by defaulting.
//! Test: `skill_floor_tests`.

use super::delegation::is_assistant_kind;

/// The server-owned FLOOR of bundled NON-CODING skill names an assistant-kind
/// agent may ever load — the ceiling `[skills].allow` narrows (#7881).
///
/// Why: the exact counterpart of
/// [`super::delegation::ASSISTANT_REACHABLE_SUBAGENTS`] for the skill
/// vocabulary. A floor is what makes `[skills].allow` safe to expose for GUI
/// editing at all: a config — or a `PATCH /api/agents/:name` write, or a
/// turn-originated `settings.patch` — that names `test-driven-development` is
/// refused by CODE, not merely absent from a curated list.
/// What: SKILL NAMES, the vocabulary `[system_prompt].skills`,
/// `[skills].allow` and `FsSkillResolver::resolve` all speak. Three sources,
/// one list:
///   - trusty-mpm's bundled catalog (`framework-manifest.toml`
///     `[skill_categories].universal`), minus everything coding-oriented —
///     engineer toolchains, cargo/build, testing, debugging, code review,
///     refactoring, API/app/DB implementation, CI security scanning.
///   - `crates/trusty-agents/.trusty-agents/skills/`, minus the language,
///     framework, testing, git-operations, docker and internal-orchestration
///     entries.
///   - the four connector/memory skills the shipped `assistant` persona
///     already declares (`gworkspace-*`, `trusty-memory-openrpc`), which
///     resolve from the operator's skill directories rather than from either
///     bundled tree.
///
/// Names are lowercase and compared exactly after trim + case-fold, matching
/// `SubagentAllowSet::resolve`'s normalization, so the two gates cannot drift
/// on spelling. Nothing is REMOVED from any catalog to achieve this — only
/// reachability changes, exactly as decision 4 did for sub-agents.
///
/// This list is hand-authored, so it is pinned against the live catalogs rather
/// than trusted: `every_bundled_skill_is_classified` reads
/// trusty-mpm's `framework-manifest.toml` and this crate's
/// `.trusty-agents/skills/` tree and FAILS on any bundled skill that is neither
/// here nor on one of the two exclusion lists beside that test. A newly bundled
/// skill therefore forces a decision instead of defaulting to unreachable.
/// Test: `every_bundled_skill_is_classified`,
/// `floor_and_exclusions_are_disjoint`,
/// `pending_owner_ruling_names_are_off_the_floor`,
/// `floor_is_either_bundled_or_declared_off_catalog`,
/// `floor_names_are_normalized_and_unique`,
/// `bundled_assistant_personas_declare_only_reachable_skills`.
pub(crate) const ASSISTANT_REACHABLE_SKILLS: &[&str] = &[
    // ---- trusty-mpm bundled, non-coding ----
    "brainstorming",
    "documentation-style",
    "internal-comms",
    "self-improvement-loop",
    "tm",
    "tm-adr",
    "tm-agent-architecture",
    "tm-bug-reporting",
    "tm-capabilities",
    "tm-circuit-breaker",
    "tm-cli-operations",
    "tm-delegation-patterns",
    "tm-doctor",
    // #8376: PM ticket-authoring skill, non-coding like `tm-issues-prune`.
    "tm-epic",
    "tm-git-file-tracking",
    "tm-init",
    "tm-issues-prune",
    "tm-postmortem",
    "tm-prose-style",
    "tm-secrets",
    "tm-session-management",
    "tm-session-pause",
    "tm-session-resume",
    "tm-slack",
    "tm-teaching-templates",
    "tm-ticketing",
    "tm-tool-usage-guide",
    "tm-verification-protocols",
    "tm-workflow",
    "verification-before-completion",
    // ---- trusty-agents bundled, non-coding ----
    "cto-apex-framework",
    "cto-bob-voice",
    "cto-db",
    "cto-duetto-org",
    "izzie-metro-north",
    "izzie-weather",
    "ticketing-epic",
    "ticketing-ticket",
    "web-search",
    // ---- connector / memory skills the shipped personas declare ----
    "gworkspace-calendar",
    "gworkspace-drive",
    "gworkspace-gmail",
    "trusty-memory-openrpc",
];

/// The effective skill names an assistant-kind agent may load.
///
/// Why: the read/dispatch half of the gate. The GUI, the CLI inspect command
/// and the three prompt-injection sites must all read ONE answer, or a pane
/// will advertise a skill the injector silently drops.
/// What: `configured` is the agent's own `[skills].allow`. `None` — no section
/// — yields the whole floor (see the module doc for why absent is not empty
/// here). `Some(list)` yields the entries of `list`, trimmed, lowercased and
/// de-duplicated, that are ALSO on the floor: config narrows, never widens, so
/// a list naming `engineer` or `rust` contributes nothing. Floor order is
/// preserved for the `None` case and caller order for the `Some` case, so the
/// output is stable to print.
/// Test: `absent_allow_list_yields_the_whole_floor`,
/// `configured_list_narrows_the_floor`,
/// `configured_list_cannot_widen_the_floor`,
/// `empty_configured_list_grants_nothing`.
pub(crate) fn reachable_skills(configured: Option<&[String]>) -> Vec<String> {
    let Some(list) = configured else {
        return ASSISTANT_REACHABLE_SKILLS
            .iter()
            .map(|s| (*s).to_string())
            .collect();
    };
    let mut out: Vec<String> = Vec::new();
    for raw in list {
        let name = raw.trim().to_ascii_lowercase();
        if !ASSISTANT_REACHABLE_SKILLS.contains(&name.as_str()) || out.contains(&name) {
            continue;
        }
        out.push(name);
    }
    out
}

/// May an agent with this `role` and this `[skills].allow` load `name`?
///
/// Why: THE enforcement point for skill injection. It is called per candidate
/// name at each of the three sites that append a skill layer to a system
/// prompt (`runtime::subagent_mode`, `agents::in_process_runner`,
/// `ctrl::ctrl_turn` — the last for BOTH the declared list and the BM25
/// dynamic matches, which is where an arbitrary coding skill could previously
/// enter an assistant's prompt from the user's own wording).
/// What: a non-assistant role is outside this rule's population entirely and
/// is always permitted — the same scope caveat
/// [`super::delegation::kind_refuses_delegation`] takes for a non-assistant
/// source. For the assistant kind, `name` must be in
/// [`reachable_skills`]; a blank name is refused before anything is looked up.
/// Test: `assistant_cannot_reach_a_coding_skill`,
/// `assistant_reaches_a_floor_skill_by_default`,
/// `non_assistant_roles_are_unaffected`, `blank_skill_name_is_refused`.
pub(crate) fn skill_is_reachable(role: &str, configured: Option<&[String]>, name: &str) -> bool {
    if !is_assistant_kind(role) {
        return true;
    }
    let name = name.trim().to_ascii_lowercase();
    if name.is_empty() {
        return false;
    }
    reachable_skills(configured).contains(&name)
}

/// Does this agent's declared `role` bind it to the skill floor?
///
/// Why: the write path reads `role` out of the raw TOML it is about to edit,
/// and a file whose `[agent].role` is missing, misspelled or not a string is
/// exactly the case that must not answer "unbound". Requirement: a floor
/// lookup that cannot be resolved REFUSES rather than defaulting to
/// everything.
/// What: `true` for the assistant kind AND for an unreadable role — fail
/// closed. `false` only when a role is present, readable, and demonstrably
/// some other kind.
/// Test: `unknown_role_is_treated_as_assistant_kind`,
/// `a_declared_worker_role_is_not_bound`.
pub(crate) fn role_is_assistant_kind_or_unknown(role: Option<&str>) -> bool {
    match role {
        Some(role) => is_assistant_kind(role),
        None => true,
    }
}

/// Narrow a caller-supplied `[skills].allow` write to the floor, reporting
/// what was refused — the WRITE-path counterpart to [`skill_is_reachable`].
///
/// Why: the `tools_allow` precedent on `PATCH /api/agents/:name` inserts the
/// caller-supplied array verbatim with no subset check; ADR-0024 decision 4's
/// ratified sub-answer (b) named that precedent as the one NOT to copy, and
/// #7881 extends the same ruling to skills. `reachable_skills` answers the
/// dispatch question one name at a time; a write endpoint needs the whole-list
/// answer BEFORE it touches the file, so the rejection is reported rather than
/// silently applied.
/// What: delegates to `tools::subagent_allow::narrow_to_floor` over
/// [`ASSISTANT_REACHABLE_SKILLS`] — one gate implementation, two floors, per
/// the crate's "no second copy of any gate" principle. `Ok(normalized)` when
/// every entry is on the floor; `Err(offenders)` with the normalized offending
/// names in input order otherwise. An empty list is a legitimate narrowing
/// ("load nothing") and is accepted.
/// Test: `narrowing_write_is_accepted`, `widening_write_is_refused`,
/// `empty_write_is_accepted`.
pub(crate) fn narrow_skills_to_floor(requested: &[String]) -> Result<Vec<String>, Vec<String>> {
    crate::tools::subagent_allow::narrow_to_floor(ASSISTANT_REACHABLE_SKILLS, requested)
}

#[cfg(test)]
#[path = "skill_floor_tests.rs"]
mod skill_floor_tests;
