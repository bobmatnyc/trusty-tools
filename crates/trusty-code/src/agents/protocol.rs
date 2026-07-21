//! `agents.*` JSON-RPC methods — the daemon's full-tier AGENT CATALOG
//! surface (issue #3449), backing the Foundry GUI's Agents management tab.
//!
//! Why: distinct from `session.get_agents` (`crate::serve::rest::agents` /
//! `crate::session::protocol_agents`), which answers "who is running (or
//! ran) inside one existing session" — this module answers "what agents
//! could I dispatch?", independent of any session. The GUI's Agents tab
//! needs to list every agent across BOTH tiers `resolve_agent` already
//! recognises (embedded ∪ disk — see [`super::resolve_agent`]'s docs) and
//! let an operator add/remove disk-tier ones; the embedded roster (32
//! agents incl. `pm`, #2958/#3437) is compiled in and therefore read-only.
//! There is exactly ONE disk tier active at a time — `<binding
//! root>/.claude/agents` when a project is bound, `~/.claude/agents`
//! (Claude-Code's user-level convention) when projectless (see
//! `crate::binding::ProjectBinding::agents_dir`) — so every disk entry this
//! module reports carries the SAME tier label for one daemon instance,
//! computed once at [`register`] time from how the daemon itself was
//! started, never re-derived per request.
//! What: [`register`] wires three methods onto a shared, immutable
//! [`AgentsCatalogState`] (`dir` + `tier`, both fixed for the daemon's
//! lifetime — no `Mutex` needed, every operation here is a stateless
//! filesystem call):
//!   - `agents.list` -> `{"agents": [{name, tier, description?, model?}]}`,
//!     the union of the embedded roster (`tier: "embedded"`) and the
//!     resolved disk tier (`tier: "project"` or `"user"`) — a disk agent
//!     overriding an embedded name WINS and suppresses the embedded copy of
//!     that name, mirroring [`super::resolve_agent`]'s disk-wins precedence
//!     exactly (so the catalog never shows the same name twice with two
//!     different tiers). An unparseable disk file surfaces as `tier:
//!     "broken"` (see [`agents_list`]'s docs) — again mirroring
//!     `resolve_agent`, whose disk-wins rule applies even to a malformed
//!     file, so dispatch of that name fails rather than falling back.
//!   - `agents.create{name, content}` -> writes
//!     `<dir>/<name>.md` from the caller-supplied Markdown+frontmatter body.
//!     Rejects (see [`validate_agent_name`]) a `name` that is empty, exceeds
//!     [`MAX_AGENT_NAME_LEN`], contains anything outside `[a-z0-9-]`, or
//!     collides with an EMBEDDED name (embedded is read-only — this endpoint
//!     never lets a disk file silently shadow it) with `-32001
//!     permission_denied` (403); an already-existing disk file of the same
//!     name with a `-32009 already_exists` conflict (409, use
//!     delete-then-create to replace).
//!   - `agents.delete{name}` -> removes `<dir>/<name>.md`. `-32002 not_found`
//!     (404) if no disk file exists under that name; `-32001
//!     permission_denied` (403) if `name` names an EMBEDDED agent (nothing
//!     to delete on disk — the embedded roster cannot be removed).
//!
//! Test: `tests::*`.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::jsonrpc::{ConnectionContext, Router, RpcError};

use super::{AgentConfig, discover_agents, load_embedded_default_agents, md_loader};

/// Maximum length for a caller-supplied agent `name` in [`agents_create`].
///
/// Why: a generous but finite bound — long enough for any real agent slug,
/// short enough to reject a pathological payload before it ever reaches the
/// filesystem.
const MAX_AGENT_NAME_LEN: usize = 64;

/// Shared, immutable state every `agents.*` handler in this module closes
/// over: the resolved disk agents directory and its tier label for this
/// daemon instance.
///
/// Why: `dir`/`tier` are fixed for the daemon's entire lifetime (derived
/// once from `ProjectBinding` at startup, see `crate::serve::build_router_at`)
/// — no shared mutable state, no `Mutex`, unlike `WorkstreamStore`.
/// What: `dir` is `ProjectBinding::agents_dir()`'s result; `tier` is
/// `"project"` when a project is bound, `"user"` when projectless.
#[derive(Clone)]
pub struct AgentsCatalogState {
    /// The resolved disk agents directory (project- or user-scoped).
    pub dir: PathBuf,
    /// `"project"` or `"user"` — which tier `dir` represents.
    pub tier: &'static str,
}

impl AgentsCatalogState {
    /// Build state from a resolved agents directory and whether a project
    /// is bound.
    ///
    /// Why: keeps the "project bound -> tier project, else user" rule in one
    /// place rather than re-deriving it at every call site that constructs
    /// this state (today: `crate::serve::build_router_at`).
    /// What: `tier = "project"` when `project_bound`, else `"user"`.
    pub fn new(dir: PathBuf, project_bound: bool) -> Self {
        Self {
            dir,
            tier: if project_bound { "project" } else { "user" },
        }
    }
}

/// Register `agents.list`, `agents.create`, `agents.delete` onto `router`.
///
/// Why: the one place listing this namespace's full surface, mirroring
/// every other `*::protocol::register` in this crate.
/// What: clones `state` once per method (cheap — `PathBuf` + `&'static
/// str`) into a small adapter closure that forwards to the corresponding
/// free function below.
/// Test: `tests::register_wires_all_three_methods`.
pub fn register(router: &mut Router, state: AgentsCatalogState) {
    let s = state.clone();
    router.register(
        "agents.list",
        move |params: Value, ctx: ConnectionContext| {
            let s = s.clone();
            async move { agents_list(&s, params, ctx).await }
        },
    );

    let s = state.clone();
    router.register(
        "agents.create",
        move |params: Value, ctx: ConnectionContext| {
            let s = s.clone();
            async move { agents_create(&s, params, ctx).await }
        },
    );

    let s = state;
    router.register(
        "agents.delete",
        move |params: Value, ctx: ConnectionContext| {
            let s = s.clone();
            async move { agents_delete(&s, params, ctx).await }
        },
    );
}

/// Whether `name` names one of the 5 `BASE-*` composition-template agents
/// (`base-agent`, `base-engineer`, `base-ops`, `base-qa`, `base-research` —
/// see [`crate::assets::BASE_AGENT_NAMES`]) rather than a real, dispatchable
/// agent.
///
/// Why: these 5 templates exist ONLY to be `extends:`-ed by concrete agents
/// (`agents::resolve_agent`/`agents::md_loader::load_md_agent` must keep
/// resolving them by name — that is how composition works) and are NEVER
/// meant to be dispatched directly. [`crate::assets::DEFAULT_AGENTS`]
/// already never includes them (Slice E3, #2958's roster decision — see
/// `assets::tests::base_templates_are_never_dispatchable`), but a real
/// project's disk `.claude/agents/` dir can legitimately contain
/// `BASE-*.md` files (trusty-mpm's own bundle installs them verbatim —
/// `crates/trusty-mpm/src/core/bundle_all.rs`), so [`agents_list`]'s disk
/// half must filter them out itself — this is that single filter, not a
/// scattered `starts_with` check per call site.
/// What: case-insensitive membership in [`crate::assets::BASE_AGENT_NAMES`]
/// — the disk filename convention is `BASE-QA.md` (uppercase stem) while
/// the frontmatter `name:`/`extends:` convention is lowercase `base-qa`, so
/// this must tolerate either.
/// Test: `tests::list_excludes_base_agents_but_resolve_agent_still_finds_them`.
fn is_base_agent(name: &str) -> bool {
    crate::assets::BASE_AGENT_NAMES.contains(&name.to_ascii_lowercase().as_str())
}

/// Wire shape for one catalog entry — shared by `agents.list`'s embedded and
/// disk halves so the two are indistinguishable to a client beyond `tier`.
fn entry_json(cfg: &AgentConfig, tier: &str) -> Value {
    json!({
        "name": cfg.agent.name,
        "tier": tier,
        "description": cfg.agent.description,
        "model": cfg.agent.model,
    })
}

/// `agents.list` -> the full embedded ∪ disk catalog.
///
/// Why: the GUI's Agents tab roster fetch — see module docs for the
/// disk-wins-over-embedded union rule.
/// What: ignores `params` (no filters yet). Builds the embedded set keyed by
/// name, then overlays disk entries, removing the embedded entry of the same
/// name whenever a disk override exists. A disk `.md` that FAILS to parse is
/// surfaced as `tier: "broken"` (description = the parse error, still
/// overriding any embedded entry of the same name) rather than skipped —
/// because [`super::resolve_agent`]'s disk-wins rule applies even to a
/// malformed file ("a disk parse error is surfaced, never silently
/// substituted", #3441), dispatch of that name WILL fail, and a catalog
/// that skipped the broken file would show the shadowed embedded entry as
/// healthy/dispatchable — lying about what `task.run` will actually do
/// (code-critic PR #3465 review, MEDIUM). A broken entry keeps the delete
/// affordance: removing the bad file is exactly the repair path. Sorted by
/// name.
///
/// The 5 `BASE-*` composition-template agents ([`is_base_agent`]) are
/// excluded from this list entirely — on either tier — because they are
/// extends-sources only and never meant to be dispatched (issue #3465
/// follow-up). This is a LISTING-only filter: [`super::resolve_agent`] and
/// `md_loader::load_md_agent` are untouched, so composition (a leaf agent's
/// `extends: base-engineer`) keeps working exactly as before — only the
/// user-facing catalog an operator browses/picks from hides them.
/// Test: `tests::list_returns_embedded_when_disk_empty`,
/// `tests::list_disk_override_wins_and_suppresses_embedded`,
/// `tests::list_disk_only_entries_are_additive`,
/// `tests::list_marks_unparseable_disk_override_as_broken`,
/// `tests::list_excludes_base_agents_but_resolve_agent_still_finds_them`.
async fn agents_list(
    state: &AgentsCatalogState,
    _params: Value,
    _ctx: ConnectionContext,
) -> Result<Value, RpcError> {
    let mut by_name: Vec<(String, Value)> = load_embedded_default_agents()
        .iter()
        .filter(|cfg| !is_base_agent(&cfg.agent.name))
        .map(|cfg| (cfg.agent.name.clone(), entry_json(cfg, "embedded")))
        .collect();

    for (name, path) in discover_agents(&state.dir) {
        if is_base_agent(&name) {
            continue;
        }
        let entry = match md_loader::load_md_agent(&path) {
            Ok(cfg) => entry_json(&cfg, state.tier),
            Err(e) => {
                tracing::warn!("agents.list: disk agent '{name}' is unparseable: {e}");
                json!({
                    "name": name,
                    "tier": "broken",
                    "description": format!("unparseable agent file — dispatch of this name will fail until it is fixed or deleted: {e}"),
                    "model": Value::Null,
                })
            }
        };
        match by_name.iter_mut().find(|(n, _)| *n == name) {
            Some(slot) => slot.1 = entry,
            None => by_name.push((name, entry)),
        }
    }

    by_name.sort_by(|a, b| a.0.cmp(&b.0));
    let agents: Vec<Value> = by_name.into_iter().map(|(_, v)| v).collect();
    Ok(json!({ "agents": agents }))
}

/// `params` shape for `agents.create`.
#[derive(Deserialize)]
struct CreateParams {
    name: String,
    content: String,
}

/// Validate a caller-supplied agent/skill name against the shared naming
/// contract (issue #3449): non-empty, `[a-z0-9-]` only, bounded length.
///
/// Why: this is ALSO the path-traversal guard — a name matching this
/// pattern can never contain `/`, `..`, or any other path-breaking
/// character, so `dir.join(format!("{name}.md"))` is always confined to
/// `dir` by construction. Shared verbatim by `agents::protocol` and
/// `skills::protocol` (issue #3449's paired endpoints) so the two surfaces
/// can never drift into two different naming rules.
/// What: `Err(RpcError::invalid_argument(..))` on any violation.
/// Test: `tests::validate_name_rejects_empty`,
/// `tests::validate_name_rejects_uppercase`,
/// `tests::validate_name_rejects_path_traversal`,
/// `tests::validate_name_rejects_too_long`,
/// `tests::validate_name_accepts_lowercase_alnum_hyphen`.
pub(crate) fn validate_agent_name(name: &str) -> Result<(), RpcError> {
    if name.is_empty() {
        return Err(RpcError::invalid_argument("name must not be empty"));
    }
    if name.len() > MAX_AGENT_NAME_LEN {
        return Err(RpcError::invalid_argument(format!(
            "name must be at most {MAX_AGENT_NAME_LEN} characters"
        )));
    }
    if !name
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err(RpcError::invalid_argument(
            "name must match [a-z0-9-]+ (lowercase letters, digits, hyphens only)",
        ));
    }
    Ok(())
}

/// `agents.create` -> write `<dir>/<name>.md` from `content`.
///
/// Why: the GUI's Agents tab add-flow — creates a new disk-tier agent.
/// What: `201`-shaped success `{"name", "tier"}` (REST layer maps this to
/// `201 Created`, see `serve::rest::agent_catalog::create`). Errors: invalid
/// `name` (`-32602`/`-32003`), `name` collides with an embedded agent
/// (`-32001 permission_denied`, 403 — embedded is read-only), an existing
/// disk file of the same name (`-32009 already_exists`, 409, use
/// delete-then-create to replace), or a write-side I/O failure (`-32603
/// internal`). The existence check is NOT a separate `path.exists()`
/// pre-check — that was a TOCTOU hole (code-critic PR #3465 review, HIGH 1:
/// two concurrent creates with the same name could both pass the check and
/// the second would silently clobber the first, violating the documented
/// 409 invariant). [`write_new_file`] makes the check-and-create a single
/// atomic `O_CREAT|O_EXCL` filesystem operation instead.
/// Test: `tests::create_writes_file_and_returns_tier`,
/// `tests::create_rejects_invalid_name`,
/// `tests::create_rejects_embedded_name_collision`,
/// `tests::create_rejects_existing_disk_file`,
/// `tests::create_conflict_does_not_clobber_existing_content`.
async fn agents_create(
    state: &AgentsCatalogState,
    params: Value,
    _ctx: ConnectionContext,
) -> Result<Value, RpcError> {
    let p: CreateParams = serde_json::from_value(params)
        .map_err(|e| RpcError::invalid_params(format!("agents.create: {e}")))?;

    validate_agent_name(&p.name)?;

    if crate::assets::DEFAULT_AGENTS
        .iter()
        .any(|a| a.name() == p.name)
    {
        return Err(RpcError::permission_denied(format!(
            "'{}' is an embedded agent and cannot be overridden by name via this endpoint",
            p.name
        )));
    }

    let path = agent_path(&state.dir, &p.name)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| RpcError::internal(format!("creating agents dir: {e}")))?;
    }
    write_new_file(&path, &p.content, || {
        format!("agent '{}' already exists on disk", p.name)
    })?;

    Ok(json!({ "name": p.name, "tier": state.tier }))
}

/// Atomically create `path` with `content`, failing with `-32009
/// already_exists` if the file already exists — the ONE way both
/// `agents.create` and `skills.create` materialise a new catalog file.
///
/// Why: a `path.exists()` pre-check followed by `std::fs::write` is a
/// TOCTOU race — two concurrent creates can both pass the check, and the
/// loser's `write` silently clobbers the winner (code-critic PR #3465
/// review, HIGH 1/HIGH 2). `OpenOptions::create_new(true)` maps to
/// `O_CREAT|O_EXCL`: the kernel makes existence-check-plus-create one
/// atomic operation, so exactly one of N racing creates succeeds and every
/// other receives `ErrorKind::AlreadyExists`, which this maps onto the same
/// `409`-shaped conflict the pre-check used to (approximately) provide.
/// What: `conflict_msg` is lazily built only on the conflict path. Any
/// other I/O failure maps to `-32603 internal`.
/// Test: `tests::create_rejects_existing_disk_file`,
/// `tests::create_conflict_does_not_clobber_existing_content`,
/// `crate::skills::protocol::tests::create_rejects_existing_disk_skill`.
pub(crate) fn write_new_file(
    path: &Path,
    content: &str,
    conflict_msg: impl FnOnce() -> String,
) -> Result<(), RpcError> {
    use std::io::Write;

    let mut file = match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(RpcError::already_exists(conflict_msg()));
        }
        Err(e) => return Err(RpcError::internal(format!("creating file: {e}"))),
    };
    file.write_all(content.as_bytes())
        .map_err(|e| RpcError::internal(format!("writing file: {e}")))?;
    Ok(())
}

/// `params` shape for `agents.delete`.
#[derive(Deserialize)]
struct DeleteParams {
    name: String,
}

/// `agents.delete` -> remove `<dir>/<name>.md`.
///
/// Why: the GUI's Agents tab remove-flow (two-step inline confirm on the
/// client side — this endpoint itself is a single, non-idempotent DELETE).
/// What: `{}` on success. `-32002 not_found` (404) when no disk file exists
/// under `name` AND it is not embedded; `-32001 permission_denied` (403)
/// when `name` names an embedded agent (nothing to delete — the embedded
/// roster is compiled in, never on disk).
/// Test: `tests::delete_removes_disk_file`,
/// `tests::delete_missing_name_returns_not_found`,
/// `tests::delete_embedded_name_returns_permission_denied`.
async fn agents_delete(
    state: &AgentsCatalogState,
    params: Value,
    _ctx: ConnectionContext,
) -> Result<Value, RpcError> {
    let p: DeleteParams = serde_json::from_value(params)
        .map_err(|e| RpcError::invalid_params(format!("agents.delete: {e}")))?;
    validate_agent_name(&p.name)?;

    let path = agent_path(&state.dir, &p.name)?;
    if !path.exists() {
        if crate::assets::DEFAULT_AGENTS
            .iter()
            .any(|a| a.name() == p.name)
        {
            return Err(RpcError::permission_denied(format!(
                "'{}' is an embedded agent and cannot be deleted",
                p.name
            )));
        }
        return Err(RpcError::not_found(format!(
            "no disk agent named '{}'",
            p.name
        )));
    }

    std::fs::remove_file(&path).map_err(|e| RpcError::internal(format!("deleting agent: {e}")))?;
    Ok(json!({}))
}

/// Join a validated `name` onto `dir` as `<dir>/<name>.md`.
///
/// Why: one seam for the join so both write paths (`create`, `delete`)
/// build the path identically; `name` must already have passed
/// [`validate_agent_name`] before this is called (it re-validates as a
/// defense-in-depth belt-and-suspenders, never trust-by-construction alone).
/// What: `Err` if `name` fails validation; otherwise `dir.join("<name>.md")`.
fn agent_path(dir: &Path, name: &str) -> Result<PathBuf, RpcError> {
    validate_agent_name(name)?;
    Ok(dir.join(format!("{name}.md")))
}

#[cfg(test)]
#[path = "protocol_tests.rs"]
mod tests;
