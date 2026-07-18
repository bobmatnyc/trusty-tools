//! `extends`-based agent personalization (DOC-41 §2.5 / §2.5.1, epic #3052,
//! issue #3055).
//!
//! Why: trusty-agents lets a user personalize a stock/bundled base agent
//! (name it, add tools, override tone) WITHOUT forking it — they author a
//! child agent declaring `extends: <base>` and layer personal deltas on top.
//! Rather than invent a bespoke algorithm, this mirrors trusty-mpm's proven
//! `compose_agent` (`trusty-mpm/src/core/agent_builder.rs`, now
//! `trusty-agents-common::agents::builder`): the SAME `MAX_DEPTH = 8` ceiling,
//! the SAME case-insensitive name resolution, and the SAME base-first prose
//! concatenation. trusty-agents does not depend on trusty-mpm as a library and
//! that composer operates on Claude-Code deploy MARKDOWN with tools-OVERRIDE
//! semantics, so this is an analogous — not shared — implementation working on
//! the structured [`AgentConfig`] with the personalization merge table.
//! What: [`resolve`] walks an agent's single-parent `extends` chain base-first
//! against a name→config lookup, enforcing cycle and depth limits, and folds
//! it into one self-contained [`AgentConfig`] via [`merge_extends`]. Resolution
//! runs once at load time (`AgentRegistry::load` / `AgentConfig::by_name`),
//! never per dispatch.
//! Test: `agents::extends::tests`.

use crate::agents::{AgentCapabilities, AgentConfig};

#[cfg(test)]
mod tests;

/// Maximum inheritance-chain depth before [`resolve`] gives up.
///
/// Why: a malformed set of agents could declare an unbounded `extends` chain;
/// the limit converts that into a clear [`AgentExtendsError::ExtendsTooDeep`]
/// instead of unbounded recursion. Mirrors `agent_builder.rs::MAX_DEPTH`
/// (DOC-41 §2.5) exactly — a safety ceiling, not an operator preference.
/// What: 8 levels of ancestry.
/// Test: `extends_depth_limit_rejected`.
pub const MAX_DEPTH: usize = 8;

/// A failure raised while resolving an `extends` inheritance chain.
///
/// Why: callers need a typed failure surface so a missing base is
/// distinguishable from a cycle from an over-deep chain, mirroring
/// `AgentBuildError` (DOC-41 §2.5) without a cross-crate dependency.
/// What: the three structural failures §2.5 enumerates.
/// Test: `extends_missing_parent_rejected`, `extends_cycle_rejected`,
/// `extends_depth_limit_rejected`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AgentExtendsError {
    /// The agent's `extends:` target could not be resolved (the informal
    /// `ExtendsTargetNotFound` in issue #3055; named `ExtendsNotFound` to
    /// match DOC-41 §2.5's normative enum).
    #[error("agent '{agent}' extends unknown base agent '{base}'")]
    ExtendsNotFound { agent: String, base: String },

    /// The `extends` chain forms a cycle; the payload is the offending chain
    /// in walk order (base-first), with the repeated name appended.
    #[error("agent inheritance cycle detected: {}", .chain.join(" -> "))]
    ExtendsCycle { chain: Vec<String> },

    /// The `extends` chain exceeded [`MAX_DEPTH`].
    #[error("agent '{agent}' extends chain exceeded depth limit of {depth}")]
    ExtendsTooDeep { agent: String, depth: usize },
}

/// Resolve an agent's `extends` chain into one flattened [`AgentConfig`].
///
/// Why: an agent declaring `extends:` is only half-defined on disk — its
/// behavior is the base's behavior with the child's deltas layered on. This
/// walks the chain to its root and folds it so downstream consumers see a
/// single self-contained config, exactly as trusty-mpm's `compose_agent`
/// flattens before a Claude Code session starts.
/// What: `name` is the agent to resolve; `lookup` returns the UNRESOLVED
/// (as-loaded, `extends` still set) config for a case-insensitive name, or
/// `None` when absent. Walks `extends` base-first, tracking a visited list for
/// cycle detection and enforcing [`MAX_DEPTH`], then folds each level via
/// [`merge_extends`]. An agent with no `extends` resolves to itself (with
/// `extends` cleared). Generic over the lookup closure so both the in-memory
/// registry map and the disk-backed `by_name` loader reuse this one walk.
/// Test: `extends_two_level_merge`, `extends_missing_parent_rejected`,
/// `extends_cycle_rejected`, `extends_depth_limit_rejected`.
pub fn resolve<F>(name: &str, lookup: &F) -> Result<AgentConfig, AgentExtendsError>
where
    F: Fn(&str) -> Option<AgentConfig>,
{
    let mut visiting: Vec<String> = Vec::new();
    resolve_inner(name, lookup, &mut visiting)
}

/// Recursive worker for [`resolve`], tracking the visited path.
///
/// Why: cycle and depth detection need the ancestry walked so far; keeping the
/// `visiting` stack in a helper keeps [`resolve`]'s public signature clean.
/// What: the depth/cycle guards mirror `agent_builder.rs::resolve` — depth is
/// checked on entry (a 9-level chain trips the limit at the 9th call), the
/// cycle check is case-insensitive to match name resolution, and the base is
/// resolved (recursively flattened) BEFORE the child is merged onto it so
/// ordering is base-first.
/// Test: exercised by every `extends_*` test via [`resolve`].
fn resolve_inner<F>(
    name: &str,
    lookup: &F,
    visiting: &mut Vec<String>,
) -> Result<AgentConfig, AgentExtendsError>
where
    F: Fn(&str) -> Option<AgentConfig>,
{
    if visiting.len() >= MAX_DEPTH {
        return Err(AgentExtendsError::ExtendsTooDeep {
            agent: name.to_string(),
            depth: MAX_DEPTH,
        });
    }
    if visiting.iter().any(|v| v.eq_ignore_ascii_case(name)) {
        let mut chain = visiting.clone();
        chain.push(name.to_string());
        return Err(AgentExtendsError::ExtendsCycle { chain });
    }

    let cfg = lookup(name).ok_or_else(|| AgentExtendsError::ExtendsNotFound {
        // The referrer is the top of the visited stack (the agent whose
        // `extends:` pointed here); for a top-level miss it is the name itself.
        agent: visiting.last().cloned().unwrap_or_else(|| name.to_string()),
        base: name.to_string(),
    })?;

    match cfg.agent.extends.clone() {
        None => Ok(clear_extends(cfg)),
        Some(base) => {
            visiting.push(name.to_string());
            let resolved_base = resolve_inner(&base, lookup, visiting)?;
            visiting.pop();
            Ok(merge_extends(resolved_base, cfg))
        }
    }
}

/// Clear the `extends` marker on a fully-resolved config.
///
/// Why: once a chain is flattened the `extends` field has been consumed;
/// leaving it set would make a resolved config look unresolved and could
/// trigger a redundant second resolution. `extends` is itself never inherited.
/// What: returns `cfg` with `agent.extends = None`.
/// Test: `extends_resolved_config_has_no_extends`.
fn clear_extends(mut cfg: AgentConfig) -> AgentConfig {
    cfg.agent.extends = None;
    cfg
}

/// Merge a resolved base config with a child overlay, base-first (DOC-41
/// §2.5 merge table).
///
/// Why: personalization layers a child's deltas onto a base's behavior. The
/// merge must be faithful to §2.5: scalars are child-overrides-when-present,
/// list fields UNION (dedup, base-first order), and prose CONCATENATES
/// base-first (parent instructions, then child) — NOT child-replaces-parent.
/// What: starts from `base` (so the child inherits the base's rich runtime
/// config — `llm`, `runner`, `compress`, `session`, `plugins`, `rbac` — which
/// a personalization overlay rarely redeclares), keeps the child's own
/// identity (`name`), then applies the §2.5 rules:
///   - scalars (`role`, `model`, `description`): child overrides when it
///     carries a non-neutral value, else inherits base (a `.md` child that
///     omits `model` leaves it empty and inherits the base's; a `.toml` child
///     always declares `model`);
///   - `display_name` / `prompt_label`: `Option` child-`Some`-wins — critically
///     this lets a child set `display_name` even when the base has NONE (owner
///     naming decision 2026-07-18: bases are nameless, the persona name comes
///     from the child);
///   - list fields (`tools.allowed`, `tools.allow`, `tools.scopes`,
///     `system_prompt.skills`, and every `capabilities` sub-list): UNION,
///     de-duplicated in first-seen base-first order;
///   - prose (`system_prompt.content`): base body then child body, joined by a
///     blank line, matching `compose_agent`'s `bodies.join("\n\n")`.
/// `extends` itself is cleared (never inherited). The `user_authority` field is
/// deliberately excluded from inheritance — see the stub note in the body.
/// The provider `adapter` is left as the base's here and recomputed by the
/// loader after model resolution.
/// Test: `extends_two_level_merge`, `extends_prose_base_first`,
/// `extends_tools_union_dedup`, `extends_child_sets_display_name_over_none`,
/// `extends_scalar_child_override`.
pub fn merge_extends(base: AgentConfig, child: AgentConfig) -> AgentConfig {
    let mut merged = base;

    // Identity always comes from the child — it is the concrete agent being
    // loaded, not the template it extends.
    merged.agent.name = child.agent.name;
    merged.agent.extends = None;

    // --- Scalars: child overrides when present (non-neutral) ---
    if !child.agent.role.is_empty() && child.agent.role != "agent" {
        merged.agent.role = child.agent.role;
    }
    if !child.agent.model.is_empty() {
        merged.agent.model = child.agent.model;
    }
    if !child.agent.description.is_empty() {
        merged.agent.description = child.agent.description;
    }
    if child.agent.display_name.is_some() {
        merged.agent.display_name = child.agent.display_name;
    }
    if child.agent.prompt_label.is_some() {
        merged.agent.prompt_label = child.agent.prompt_label;
    }

    // --- user_authority: NEVER inherited/unioned through `extends` ---
    //
    // Owner decision 2026-07-18 (AUTH carve-out): a child's `user_authority` is
    // always its own explicit setting (defaulting false), even when `extends`
    // targets the authority holder. The field does not exist on `AgentConfig`
    // yet — AUTH-1/#3074 adds it. This is the merge site where the exclusion
    // must live: when the field lands, DO NOT copy it from `base`; leave
    // `merged`'s value (which will already be the child's own, since `merged`
    // starts from `base` — so AUTH-1 must additionally reset it to the child's
    // explicit value here). See the TODO test placeholder in `tests.rs`
    // (`extends_does_not_inherit_user_authority`) for AUTH-2/#3075 to fill in.

    // --- List fields: union (dedup, base-first order) ---
    merged.tools.allowed = union_opt_vec(merged.tools.allowed, child.tools.allowed);
    merged.tools.allow = union_opt_vec(merged.tools.allow, child.tools.allow);
    merged.tools.scopes = union_opt_vec(merged.tools.scopes, child.tools.scopes);
    merged.system_prompt.skills =
        union_opt_vec(merged.system_prompt.skills, child.system_prompt.skills);
    merged.agent.capabilities =
        merge_capabilities(merged.agent.capabilities, child.agent.capabilities);

    // --- Prose: base-first concatenation ---
    merged.system_prompt.content =
        concat_prose(&merged.system_prompt.content, &child.system_prompt.content);

    merged
}

/// Union two optional string lists, de-duplicated in first-seen (base-first)
/// order.
///
/// Why: list fields accumulate through inheritance (a child ADDS tools to the
/// base set rather than replacing it) — the personalization semantics of
/// DOC-41 §2.5, and the key point that distinguishes trusty-agents' `tools`
/// union from trusty-mpm's `tools` override.
/// What: `None + None => None`; otherwise `Some` of the base list followed by
/// any child entries not already present (case-sensitive dedup — tool/scope
/// names are case-sensitive identifiers). Preserves base-first order.
/// Test: `extends_tools_union_dedup`, `extends_union_none_cases`.
fn union_opt_vec(base: Option<Vec<String>>, child: Option<Vec<String>>) -> Option<Vec<String>> {
    match (base, child) {
        (None, None) => None,
        (Some(b), None) => Some(b),
        (None, Some(c)) => Some(c),
        (Some(mut b), Some(c)) => {
            for item in c {
                if !b.contains(&item) {
                    b.push(item);
                }
            }
            Some(b)
        }
    }
}

/// Union the four capability sub-lists of two optional [`AgentCapabilities`].
///
/// Why: `capabilities` is a list-bearing field, so it follows the same union
/// rule as `tools` — a child's declared languages/frameworks/roles/tags add to
/// the base's rather than replacing them.
/// What: `None + None => None`; otherwise `Some` with each of the four lists
/// unioned base-first via a plain `Vec<String>` union (dedup, first-seen order).
/// Test: `extends_capabilities_union`.
fn merge_capabilities(
    base: Option<AgentCapabilities>,
    child: Option<AgentCapabilities>,
) -> Option<AgentCapabilities> {
    match (base, child) {
        (None, None) => None,
        (Some(b), None) => Some(b),
        (None, Some(c)) => Some(c),
        (Some(b), Some(c)) => Some(AgentCapabilities {
            languages: union_vec(b.languages, c.languages),
            frameworks: union_vec(b.frameworks, c.frameworks),
            roles: union_vec(b.roles, c.roles),
            tags: union_vec(b.tags, c.tags),
        }),
    }
}

/// Union two `Vec<String>` de-duplicated in first-seen (base-first) order.
///
/// Why: the bare-`Vec` counterpart of [`union_opt_vec`] for `capabilities`
/// sub-lists, which are non-optional `Vec`s.
/// What: base entries, then child entries not already present.
/// Test: covered via `extends_capabilities_union`.
fn union_vec(mut base: Vec<String>, child: Vec<String>) -> Vec<String> {
    for item in child {
        if !base.contains(&item) {
            base.push(item);
        }
    }
    base
}

/// Concatenate base and child prose base-first, joined by a blank line.
///
/// Why: §2.5 mandates base-first prose concatenation (parent instructions
/// then child), matching `compose_agent`'s `bodies.join("\n\n")` — NOT
/// child-replaces-parent. A child's persona/override prose is APPENDED so the
/// base's behavior is preserved and refined, not discarded.
/// What: trims surrounding newlines off each side, drops empties, and joins
/// the non-empty parts with `"\n\n"`.
/// Test: `extends_prose_base_first`, `extends_prose_empty_sides`.
fn concat_prose(base: &str, child: &str) -> String {
    let parts: Vec<&str> = [base, child]
        .into_iter()
        .map(|s| s.trim_matches('\n'))
        .filter(|s| !s.is_empty())
        .collect();
    parts.join("\n\n")
}
