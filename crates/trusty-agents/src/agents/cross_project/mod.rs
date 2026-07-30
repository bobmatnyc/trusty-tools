//! L0-only cross-project git/search scoping (#4172, epic #4167).
//!
//! Why: The owner's tiered persona model (#4167, resolver landed in #4200)
//! gives L0 ("orchestration") the PM-tier reach an L1 ("standard") assistant
//! must never have. One of those grants is **cross-project reach**: an
//! orchestration persona reconciling a snapshot against live state has to
//! read git status across several repos and search corpora outside its own
//! project, while every L1 persona stays exactly as single-tenant as it is
//! today.
//!
//! **The one-store rule is NOT relaxed — for either tier.** DOC-54 §5.1
//! ("Stores (Knowledge): One per agent", Bob 2026-07-24) governs
//! `[[stores]]`: the agent's ONE dedicated, curated OKG store — its OKF
//! knowledge tree plus the search index over that tree. That ceiling, and
//! the `extends` replace-not-union rule that protects it
//! (`crate::agents::extends`), are untouched by this module: nothing here
//! reads, writes, unions, or widens `[[stores]]`.
//!
//! The multiplicity #4172 asks for already has a sanctioned home. Epic
//! #4007's tier-2 mechanism — `[tools].search_indexes` (#3232) — exists
//! precisely for "also let me search the APEX repo", and its own field docs
//! (`ToolsConfig::search_indexes`) state the split: "`[[stores]]` is the
//! CURATED tier — exactly one OKG store per agent by the 2026-07-24 owner
//! decision, and that ceiling stays. The multiplicity an agent needs …
//! belongs to a second, uncurated tier." So cross-project *reach* is a
//! tier-2 + filesystem question, not a `[[stores]]` question, and #4172 is
//! implementable without touching a normative constraint.
//!
//! What: [`CrossProjectScope`] — the single resolution point for "which repo
//! roots and which search indexes may THIS agent touch". It is built from a
//! real [`AgentInfo`] (so the tier comes from #4200's fail-closed
//! `AgentInfo::tier`, never a local guess) plus the agent's home project
//! root, and it answers two questions:
//!
//! - [`CrossProjectScope::resolve_repo_root`] — turn an optional caller-
//!   supplied `repo` path into a permitted absolute root, or refuse.
//! - [`CrossProjectScope::effective_search_indexes`] — widen an agent's
//!   declared tier-2 index list with the cross-project index ids it may
//!   query.
//!
//! **The bound (requirement: not "any path on disk").** An L0 agent's
//! allow-set is:
//!
//! > its own home project root, PLUS every entry of the trusty-agents
//! > project registry (`~/.trusty-agents/projects.json`) that is
//! > simultaneously (a) `ProjectStatus::Active`, (b)
//! > `ProjectEntry::is_real_project()` (no temp dirs), (c) an existing
//! > directory, and (d) a git repository root (carries a `.git` entry).
//!
//! Every one of those four conditions is mechanical and re-checked at
//! resolution time; a path satisfying none of them is unreachable. An L1
//! agent's allow-set is its home root and nothing else — byte-identically
//! today's behaviour. Containment is component-wise
//! ([`Path::starts_with`]) against CANONICALIZED roots, so `..` traversal
//! and symlink indirection cannot smuggle a path in, and a sibling
//! directory that merely shares a string prefix (`/a/bc` vs `/a/b`) is not
//! a match.
//!
//! Auditability: [`CrossProjectScope::audit_summary`] renders the resolved
//! tier and full allow-set as one line, emitted at `tracing::info` on
//! construction; every refusal is emitted at `tracing::warn` and returned
//! to the model as an error naming the allow-set.
//!
//! Why the trusty-agents registry and not trusty-mpm's: trusty-mpm's
//! curated project registry is unreachable from this crate as Rust types
//! (`trusty-agents` has no `trusty-mpm` dependency — the sanctioned access
//! pattern is a loopback HTTP GET, per `crate::system_status::daemons`), it
//! carries **no local filesystem path** at all (records are keyed on
//! `name`/`repo_url`), and a local path can only be *fabricated* from it by
//! the `~/trusty-mpm-projects/<owner>/<repo>` layout convention. A security
//! allow-set must not depend on a sibling daemon being up, nor on a
//! conventionally-guessed path: a wedged daemon would silently change the
//! boundary. The in-crate registry is on-disk, deterministic, inspectable by
//! the user (`list_projects`), and already documented as "the single source
//! of truth for 'what projects does this user have?'" (`crate::registry`).
//!
//! No credential surface is widened here: this module resolves *paths* and
//! *index ids* only. Cross-project git operations run through the same
//! ambient git credential helper the home project already uses — no
//! per-project secret is read, copied, or forwarded.
//!
//! Test: `l1_scope_allows_only_its_home_root`,
//! `l0_scope_includes_registered_git_projects`,
//! `l0_resolve_repo_root_refuses_unregistered_sibling`,
//! `unrecognized_tier_declaration_resolves_to_single_tenant_scope`,
//! `undeclared_tier_follows_the_kind_derivation`.

#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};

use crate::agents::{AgentInfo, AgentTier};
use crate::registry::{ProjectEntry, ProjectRegistry, ProjectStatus};

/// The resolved cross-project reach of ONE agent (#4172).
///
/// Why: Both consumers — the git tool surface and `vector_search`'s tier-2
/// index allowlist — must agree on what "this agent's projects" means. One
/// value carrying the resolved tier plus the canonicalized allow-set is what
/// keeps them from drifting, and is what makes the boundary auditable in a
/// single log line.
/// What: An immutable value. `allowed_roots` always begins with the home
/// root; for [`AgentTier::L1Standard`] it contains ONLY the home root.
/// `cross_project_indexes` is empty for L1.
/// Test: `l1_scope_allows_only_its_home_root`,
/// `l0_scope_includes_registered_git_projects`.
#[derive(Debug, Clone)]
pub struct CrossProjectScope {
    /// The tier this scope was resolved for, straight from
    /// [`AgentInfo::tier`] (#4200) — never re-derived locally.
    tier: AgentTier,
    /// The agent's own project root, VERBATIM as the construction site
    /// supplied it.
    ///
    /// Why: This is what a tool bound to this scope acts on when the caller
    /// names no `repo`, so it must be the byte-identical value the
    /// pre-#4172 call site passed to `git_tools(root)` — canonicalizing it
    /// here would silently retarget every existing registration (on macOS
    /// `/tmp/x` becomes `/private/tmp/x`). Containment comparisons use
    /// `allowed_roots`, which IS canonicalized, so the two concerns stay
    /// separate.
    home_root: PathBuf,
    /// Canonicalized roots this agent may operate in. Canonical home root
    /// first, then (L0 only) the admitted registry projects in registry
    /// order.
    allowed_roots: Vec<PathBuf>,
    /// trusty-search index ids for the non-home allowed roots (L0 only),
    /// derived by [`trusty_common::derive_index_id`].
    cross_project_indexes: Vec<String>,
}

impl CrossProjectScope {
    /// The single-tenant scope: one root, no cross-project reach (#4172).
    ///
    /// Why: This is BOTH the L1 shape and the pre-#4172 behaviour of every
    /// existing call site. `git_tools(root)` keeps its signature by
    /// delegating through this constructor, so no path that has not been
    /// deliberately migrated changes behaviour at all.
    /// What: Tier is the fail-closed [`AgentTier::L1Standard`]; the allow-set
    /// is exactly `[canonical(root)]`; no cross-project indexes.
    /// Test: `single_tenant_scope_is_not_cross_project`,
    /// `l1_scope_allows_only_its_home_root`.
    pub fn single_tenant(root: PathBuf) -> Self {
        Self {
            tier: AgentTier::L1Standard,
            allowed_roots: vec![canonical_or_verbatim(&root)],
            home_root: root,
            cross_project_indexes: Vec::new(),
        }
    }

    /// Resolve an agent's scope against an already-loaded registry snapshot
    /// (#4172).
    ///
    /// Why: The pure core, so the boundary is testable without touching
    /// `$HOME` or the real `projects.json`. [`Self::from_registry`] is the
    /// thin I/O wrapper over it.
    /// What: Reads the tier via [`AgentInfo::tier`] — #4200's fail-closed
    /// accessor, so an absent, blank, or unrecognized `tier` value lands on
    /// [`AgentTier::L1Standard`] and this returns exactly
    /// [`Self::single_tenant`]. For [`AgentTier::L0Orchestration`], admits
    /// each `entries` record passing all four conditions in the module docs,
    /// skipping the home root itself and any duplicate.
    /// Test: `l0_scope_includes_registered_git_projects`,
    /// `l0_scope_excludes_non_git_and_inactive_and_temp_entries`,
    /// `unrecognized_tier_declaration_resolves_to_single_tenant_scope`,
    /// `undeclared_tier_follows_the_kind_derivation`.
    pub fn resolve(agent: &AgentInfo, home_root: &Path, entries: &[ProjectEntry]) -> Self {
        let tier = agent.tier();
        let canonical_home = canonical_or_verbatim(home_root);
        let home_root = home_root.to_path_buf();
        if tier != AgentTier::L0Orchestration {
            let scope = Self {
                tier,
                allowed_roots: vec![canonical_home],
                home_root,
                cross_project_indexes: Vec::new(),
            };
            tracing::info!(target: "trusty_agents::cross_project", "{}", scope.audit_summary());
            return scope;
        }

        let mut allowed_roots = vec![canonical_home];
        let mut cross_project_indexes: Vec<String> = Vec::new();
        for entry in entries {
            if entry.status != ProjectStatus::Active || !entry.is_real_project() {
                continue;
            }
            if !is_git_repo_root(&entry.path) {
                continue;
            }
            let root = canonical_or_verbatim(&entry.path);
            if allowed_roots.contains(&root) {
                continue;
            }
            let index_id = trusty_common::derive_index_id(&root);
            allowed_roots.push(root);
            if !index_id.is_empty() && !cross_project_indexes.contains(&index_id) {
                cross_project_indexes.push(index_id);
            }
        }

        let scope = Self {
            tier,
            home_root,
            allowed_roots,
            cross_project_indexes,
        };
        tracing::info!(target: "trusty_agents::cross_project", "{}", scope.audit_summary());
        scope
    }

    /// Resolve an agent's scope, loading the on-disk project registry
    /// (#4172).
    ///
    /// Why: The production entry point. Registry I/O can only ever SHRINK
    /// the allow-set: no `$HOME`, a missing file, or malformed JSON all yield
    /// an EMPTY snapshot, which collapses even an L0 agent to its home root.
    /// Failing closed on a read problem is the only safe direction for a
    /// privilege boundary. (`ProjectRegistry::list_active` already degrades a
    /// parse failure to an empty map itself; the `unwrap_or_else` below is
    /// belt-and-braces for any error it does surface.)
    /// What: `ProjectRegistry::new().list_active()`, then [`Self::resolve`].
    /// Test: `from_registry_fails_closed_when_registry_is_unreadable`,
    /// `from_registry_loads_active_entries_for_l0`.
    pub async fn from_registry(agent: &AgentInfo, home_root: &Path) -> Self {
        let entries = match ProjectRegistry::new() {
            Ok(reg) => reg.list_active().await.unwrap_or_else(|e| {
                tracing::warn!(
                    target: "trusty_agents::cross_project",
                    "project registry unreadable ({e:#}); cross-project reach denied"
                );
                Vec::new()
            }),
            Err(e) => {
                tracing::warn!(
                    target: "trusty_agents::cross_project",
                    "project registry unavailable ({e:#}); cross-project reach denied"
                );
                Vec::new()
            }
        };
        Self::resolve(agent, home_root, &entries)
    }

    /// Same as [`Self::from_registry`] but against an explicit registry file
    /// (#4172).
    ///
    /// Why: `ProjectRegistry::new()` hardcodes `$HOME`, and mutating `HOME`
    /// from a test is process-global and races every other test in the
    /// binary. An explicit path lets the boundary tests drive the REAL
    /// registry loader over a real `projects.json` in a tempdir, rather than
    /// hand-building `ProjectEntry` values and skipping the loader entirely.
    /// What: Identical to [`Self::from_registry`], including its fail-closed
    /// error handling, but reads `registry_path`.
    /// Test: `from_registry_loads_active_entries_for_l0`,
    /// `from_registry_fails_closed_when_registry_is_unreadable`.
    pub async fn from_registry_path(
        agent: &AgentInfo,
        home_root: &Path,
        registry_path: PathBuf,
    ) -> Self {
        let entries = ProjectRegistry::with_registry_path(registry_path)
            .list_active()
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(
                    target: "trusty_agents::cross_project",
                    "project registry unreadable ({e:#}); cross-project reach denied"
                );
                Vec::new()
            });
        Self::resolve(agent, home_root, &entries)
    }

    /// The tier this scope was resolved for.
    ///
    /// Test: `unrecognized_tier_declaration_resolves_to_single_tenant_scope`,
    /// `undeclared_tier_follows_the_kind_derivation`.
    pub fn tier(&self) -> AgentTier {
        self.tier
    }

    /// The agent's own project root — the default target of every tool bound
    /// to this scope.
    ///
    /// Test: `l1_scope_allows_only_its_home_root`.
    pub fn home_root(&self) -> &Path {
        &self.home_root
    }

    /// Every root this agent may operate in, home root first.
    ///
    /// Test: `l0_scope_includes_registered_git_projects`.
    pub fn allowed_roots(&self) -> &[PathBuf] {
        &self.allowed_roots
    }

    /// Whether this scope reaches beyond its home root at all.
    ///
    /// Why: The git tool schemas advertise a `repo` argument ONLY when this
    /// is true, so an L1 persona's tool surface is unchanged down to the
    /// JSON schema — the model is never shown a knob it cannot turn.
    /// What: True exactly when more than one root is permitted (L0 with at
    /// least one admitted registry project).
    /// Test: `single_tenant_scope_is_not_cross_project`,
    /// `l0_scope_includes_registered_git_projects`.
    pub fn is_cross_project(&self) -> bool {
        self.allowed_roots.len() > 1
    }

    /// The cross-project trusty-search index ids this agent may query
    /// (#4172, epic #4007 tier-2).
    ///
    /// Test: `l0_cross_project_indexes_are_derived_from_allowed_roots`.
    pub fn cross_project_indexes(&self) -> &[String] {
        &self.cross_project_indexes
    }

    /// Widen an agent's DECLARED tier-2 index list with its cross-project
    /// reach (#4172).
    ///
    /// Why: `vector_search` derives both its schema enumeration and its
    /// (opt-in) fail-closed allowlist from one list
    /// (`VectorSearchTool::allowed_index_ids`). Folding cross-project ids in
    /// at that one input is what makes "L0 can search other projects' code"
    /// true without a second, divergent allowlist. Note the asymmetry this
    /// preserves: `[[stores]]` — the ONE curated OKG store, DOC-54 §5.1 — is
    /// not consulted or altered here; only the uncurated tier-2 list is
    /// widened.
    /// What: For any tier but L0, returns `declared` verbatim — byte-
    /// identical to pre-#4172. For L0, appends the cross-project ids after
    /// the declared ones (declaration order preserved, dedup, blanks
    /// dropped) so an explicitly declared index always keeps its priority.
    /// Test: `l1_effective_search_indexes_are_declared_list_verbatim`,
    /// `l0_effective_search_indexes_append_cross_project_ids`,
    /// `l0_effective_search_indexes_do_not_duplicate_declared_ids`.
    pub fn effective_search_indexes(&self, declared: Vec<String>) -> Vec<String> {
        if self.tier != AgentTier::L0Orchestration {
            return declared;
        }
        let mut out: Vec<String> = Vec::new();
        for id in declared
            .iter()
            .map(String::as_str)
            .chain(self.cross_project_indexes.iter().map(String::as_str))
        {
            let id = id.trim();
            if id.is_empty() || out.iter().any(|e| e == id) {
                continue;
            }
            out.push(id.to_string());
        }
        out
    }

    /// Resolve a caller-supplied `repo` into a permitted absolute root, or
    /// refuse (#4172).
    ///
    /// Why: This is THE enforcement point for the filesystem half of #4172 —
    /// the one function every scoped tool calls, so "bounded to the
    /// allow-set" is a property of one reviewable check rather than of
    /// twelve tool bodies.
    /// What: `None` / blank → the home root. Otherwise the path must be
    /// ABSOLUTE (a relative path is refused rather than joined, so the
    /// process cwd can never move the boundary), must canonicalize (an
    /// unresolvable path cannot be PROVEN contained, so it is refused), and
    /// must then be equal to or a component-wise descendant of one of
    /// [`Self::allowed_roots`]. Canonicalizing first is what defeats `..`
    /// traversal and symlinks; component-wise [`Path::starts_with`] is what
    /// stops `/a/bc` from matching root `/a/b`. Every refusal is logged and
    /// names the allow-set.
    /// Test: `resolve_repo_root_defaults_to_home_root`,
    /// `l1_resolve_repo_root_refuses_another_project`,
    /// `l0_resolve_repo_root_accepts_registered_project`,
    /// `l0_resolve_repo_root_accepts_subdirectory_of_registered_project`,
    /// `l0_resolve_repo_root_refuses_unregistered_sibling`,
    /// `l0_resolve_repo_root_refuses_parent_traversal_escape`,
    /// `l0_resolve_repo_root_refuses_symlink_escape`,
    /// `resolve_repo_root_refuses_relative_path`,
    /// `resolve_repo_root_refuses_prefix_lookalike_sibling`.
    pub fn resolve_repo_root(&self, requested: Option<&str>) -> Result<PathBuf, String> {
        let Some(raw) = requested.map(str::trim).filter(|s| !s.is_empty()) else {
            return Ok(self.home_root.clone());
        };
        let candidate = Path::new(raw);
        if !candidate.is_absolute() {
            return Err(self.denial(raw, "path is not absolute"));
        }
        let Ok(resolved) = candidate.canonicalize() else {
            return Err(self.denial(raw, "path does not resolve to an existing directory"));
        };
        if self.permits(&resolved) {
            return Ok(resolved);
        }
        Err(self.denial(raw, "path is outside this agent's project allow-set"))
    }

    /// Whether `path` lies inside this scope's allow-set (#4172).
    ///
    /// Why: A SECOND enforcement need that [`Self::resolve_repo_root`] alone
    /// cannot cover. `crate::git::GitRepo::open` uses libgit2's
    /// `Repository::discover`, which walks UPWARD from its seed — so a
    /// permitted seed path can still resolve to a repository whose work tree
    /// is an ANCESTOR outside the allow-set. The git tool surface re-checks
    /// the discovered work tree through this predicate before acting on it,
    /// closing that hop.
    /// What: Canonicalizes `path` (falling back to it verbatim) and tests
    /// component-wise containment against every allowed root.
    /// Test: `permits_accepts_home_root_and_descendants`,
    /// `l0_permits_registered_project_but_not_its_parent`.
    pub fn permits(&self, path: &Path) -> bool {
        let resolved = canonical_or_verbatim(path);
        self.allowed_roots.iter().any(|r| resolved.starts_with(r))
    }

    /// One-line audit record of this scope's resolved boundary (#4172).
    ///
    /// Why: Requirement that the widening be auditable. Emitted at
    /// construction so a session log always states which roots an agent was
    /// granted, and reused verbatim in every refusal message so the model —
    /// and the operator reading the transcript — sees the same allow-set the
    /// gate applied.
    /// What: `tier=<L0|L1> cross_project=<bool> roots=[…] indexes=[…]`.
    /// Test: `audit_summary_names_tier_and_allowed_roots`.
    pub fn audit_summary(&self) -> String {
        let tier = match self.tier {
            AgentTier::L0Orchestration => "L0Orchestration",
            AgentTier::L1Standard => "L1Standard",
        };
        let roots = self
            .allowed_roots
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "cross-project scope: tier={tier} cross_project={} roots=[{roots}] indexes=[{}]",
            self.is_cross_project(),
            self.cross_project_indexes.join(", ")
        )
    }

    /// Build (and log) the refusal message for a rejected `repo` argument.
    ///
    /// Why: Keeps the `tracing::warn!` audit line and the model-facing error
    /// string in lockstep — a denial that is reported to the model but not
    /// logged (or vice versa) is exactly the gap "auditable" has to close.
    /// What: Logs at warn and returns the same text.
    /// Test: `l1_resolve_repo_root_refuses_another_project`.
    fn denial(&self, raw: &str, reason: &str) -> String {
        let msg = format!("repo '{raw}' denied: {reason}. {}", self.audit_summary());
        tracing::warn!(target: "trusty_agents::cross_project", "{msg}");
        msg
    }
}

/// Canonicalize `path`, falling back to it verbatim.
///
/// Why: The allow-set and the candidate must be compared in the SAME
/// representation or containment is meaningless (macOS APFS is
/// case-insensitive/case-preserving, and a symlinked home alias reaches the
/// same directory by a different string on every platform). Canonicalizing
/// both sides collapses those to one form. A root that cannot be
/// canonicalized (deleted between registry write and now) is kept verbatim
/// rather than dropped, which is safe in the restrictive direction: a
/// candidate IS canonicalized, so it can only ever match a verbatim root
/// that was already canonical.
/// What: `std::fs::canonicalize` or the input.
/// Test: `l0_resolve_repo_root_accepts_registered_project`,
/// `l0_resolve_repo_root_refuses_symlink_escape`.
fn canonical_or_verbatim(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Whether `path` is the root of a git repository.
///
/// Why: Condition (d) of the allow-set. A registry entry is a directory the
/// user connected trusty-agents to; requiring a `.git` entry keeps the
/// git-tool allow-set to actual repositories instead of every such
/// directory, and matches the "must be a real git checkout" bar
/// `trusty-code`'s own project roster already applies.
/// What: `path.join(".git").exists()` — true for both a normal repo
/// (directory) and a worktree/submodule (gitfile), which is deliberate:
/// `.base/.worktrees/*` checkouts are exactly the roots an orchestration
/// persona works in.
/// Test: `l0_scope_excludes_non_git_and_inactive_and_temp_entries`.
fn is_git_repo_root(path: &Path) -> bool {
    path.join(".git").exists()
}
