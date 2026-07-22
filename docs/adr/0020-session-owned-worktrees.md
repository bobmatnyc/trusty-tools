# 0020. Session-owned worktrees: ownership registry + owner-gated reclamation

- **Status:** Accepted
- **Date:** 2026-07-22
- **Scope:** crate `trusty-mpm` (`session_manager`, `provisioner::workspace`, `daemon::managed_routes::inproject`, orphan-GC)
- **Reversibility Cost:** Low — purely additive (`#[serde(default)]` fields, a JSON sentinel payload replacing a zero-byte convention); no destructive migration, no schema break
- **Decision Drivers:** worktree data-loss risk from unattributed reclamation, multiple independent worktree stores with no ownership concept, safe-default bias (never delete over accidentally deleting live work)
- **Supersedes / Superseded by:** none

## Context

trusty-mpm provisions per-session git worktrees in **three** independent on-disk shapes, and until this change none of them recorded WHO is entitled to reclaim a given worktree:

1. **`<project_dir>/.base/.worktrees/<session-id>/`** — the clone-based shared-base-checkout path, created by `WorkspaceProvisioner::provision_in` (`crates/trusty-mpm/src/provisioner/workspace.rs`). A bare `.trusty-mpm-worktree` sentinel file was written here (zero bytes) purely to mark "an SM process created this directory."
2. **`~/trusty-mpm-projects/<owner>/<repo>/.worktrees/<name>/`** — the in-project spawn path, created by `daemon::managed_routes::inproject::create_session_worktree`. Same zero-byte sentinel convention.
3. **`.claude/worktrees/`** — a harness (Claude Code agent-fork) worktree store, entirely independent of trusty-mpm's session manager. **Out of scope for this ADR's code changes** — see "Out of scope" below.

Two independent teardown paths could remove a worktree from shape (1) or (2):

- **`SessionManager::decommission`**, invoked by an operator, the CLI, an HTTP route, or an MCP tool, given a specific session id.
- **The orphan-GC sweep** (`SessionManager::reap_orphaned_worktrees` / `prune_orphaned_worktrees`, run on a daemon timer), which walked the `.worktrees/<name>` shape ONLY (never `.base/.worktrees`), and deleted ANY directory not currently claimed by a live session's `workspace_path` — with no concept of "does this directory even belong to a session that still exists, and is it done with it."

Neither path asked "who owns this worktree, and are they still using it?" This is not hypothetical: during this issue's own implementation, the orphan-GC sweep on the SHARED daemon (running independently of this session) deleted this session's own working worktree directory twice, because the directory sat directly under the exact `<repos_root>/<owner>/<repo>/.worktrees/<name>` shape the sweep scans, and — pre-#3649 — carried no ownership marker distinguishing it from a genuinely abandoned directory. This is precisely the failure class this ADR closes.

A second, related gap: `decommission` had no notion of a "caller" identity at all — any process invoking it (including, over MCP, one managed session acting on behalf of another) could tear down any OTHER session's worktree with no ownership check whatsoever.

## Decision

We will add a lightweight, additive **ownership registry** — Option B from the issue's design discussion — rather than a heavier redesign (e.g., a distributed lock service, or migrating all three stores to a single shape). Concretely:

1. **Sentinel payload.** The `.trusty-mpm-worktree` sentinel file (unchanged path/name) now carries a small JSON payload — `{"owner_session_id": "<uuid>", "created_at": "<rfc3339>"}` — instead of zero bytes, written at both real provisioning call sites (`provisioner::workspace::WorkspaceProvisioner::provision_in`, `daemon::managed_routes::inproject::create_session_worktree`). The parser (`session_manager::worktree_ownership::read_sentinel_owner`) is **tolerant by construction**: an absent file, an empty (legacy zero-byte) file, or unparsable content all resolve to `SentinelOwner::Unknown` — never an error, and never treated as "safe to delete."

2. **Registry field.** `SessionRecord` gains `#[serde(default)] pub worktree_owner: Option<ManagedSessionId>`, set to the session's own id via a new post-creation setter, `SessionManager::set_worktree_owner`, called immediately after `create_with_id`/`create_with_reserved_name` succeeds in both real provisioning call sites (`daemon::managed_routes::lifecycle::spawn_managed_cloned` and `spawn_managed_inproject`). We chose a **post-creation setter** (mirroring the existing `set_workspace_owned` precedent) over threading a new parameter through the ~19 `create_with_id`/`create_with_reserved_name` call sites, trading a small, best-effort race window (a record is briefly owner-unknown between creation and the setter call) for far less blast radius across the codebase. `None` means legacy/owner-unknown — the `#[serde(default)]` behavior for every pre-#3649 record.

3. **Owner-gated decommission.** `SessionManager::decommission` / `decommission_with_root` gained an `caller: Option<ManagedSessionId>` parameter. When `caller` is `Some(c)` and the target's worktree has a KNOWN owner `o` (from the registry field, falling back to the on-disk sentinel if the field itself is unset), the call is refused with a typed `ManagedError::WorktreeOwnerMismatch { caller, owner, target }` unless `c == o` OR the owner is provably ownerless (below). When `caller` is `None` — every CLI, HTTP-route, and daemon-internal call site (the age-based reaper, bulk prune, `dedup`) — current authority is preserved unconditionally; this is the assumed identity of an operator or the daemon itself, never subject to the gate. The MCP `session_decommission`/`session_prune` tools also thread the parameter through to `prune_managed`, but currently always pass `None`: the MCP wire protocol has no per-connection caller identity yet (tracked as follow-up work; `TM_MANAGED_SESSION_ID` is a candidate future source).

4. **Owner-gated orphan-GC.** `find_orphaned_worktrees` now walks BOTH worktree-store shapes under each `<repos_root>/<owner>/<repo>/` — the pre-existing `.worktrees/<name>` shape AND the previously-invisible `.base/.worktrees/<session-id>` shape. Before any candidate is actually deleted, `prune_orphaned_worktrees` reads its ownership sentinel: **owner-unknown candidates are NEVER auto-deleted** — they are counted in the new `OrphanSweepOutcome::owner_unknown` and logged, so they keep surfacing via the daemon's orphan-GC log line, `tm session prune-worktrees --dry-run`, and the existing `tm doctor` worktree-health probe (which reuses `find_orphaned_worktrees` unchanged) until a human acts. A KNOWN-owner candidate is only reclaimed if the owner is **provably ownerless** (below) AND `git worktree list` on the owning checkout agrees the path is a real, still-registered worktree (a disagreement is skipped conservatively).

5. **Provably ownerless.** `SessionManager::resolve_ownerless(owner)` returns `true` only when the owner's session record does not resolve in the store at all (deleted/never-registered), or resolves to a record in a TERMINAL state (`Decommissioned`/`Deleted`, via `ManagedSessionState::is_terminal`). A live, `Stopped` (resumable), or `Errored` owner's worktree is **never** ownerless. This single predicate is shared by both the decommission gate and the orphan-GC sweep so they can never disagree on what "ownerless" means.

6. **Zero migration.** No background job rewrites legacy sentinels or backfills `worktree_owner` on old records. A legacy worktree stays owner-unknown forever unless the session that created it is re-provisioned under the new scheme. This is treated as a FEATURE, not a gap: it means the safe default (never auto-delete an unattributed worktree) applies retroactively to every worktree that existed before this ADR, with zero risk of a migration script mis-attributing ownership. Legacy worktrees remain visible and actionable via the existing `prune --dry-run` / `tm doctor` flows, exactly as before this ADR — this ADR only makes NEW worktrees ownership-aware; it never makes an old one less safe.

### Out of scope: the harness `.claude/worktrees/` store

The third worktree store — created by the Claude Code harness itself for agent-forked sub-tasks (`Agent(subagent_type: ..., isolation: "worktree")`) — is **not** touched by this ADR. It is a different lifecycle owner (the harness process, not `SessionManager`) with its own cleanup semantics. We reserve the SAME sentinel filename/shape (`.trusty-mpm-worktree` with the JSON payload) as forward-compatible in case a future ADR unifies ownership tracking across all three stores, but no code in this change reads, writes, or reasons about that shape.

## Consequences

### Positive

- The exact failure class observed during this issue's own implementation — an unattributed worktree directory deleted by a sibling daemon's orphan-GC sweep — is now structurally prevented for any worktree provisioned after this change: it always carries a resolvable owner, and the owner's liveness is checked before deletion.
- `find_orphaned_worktrees` now covers the `.base/.worktrees` shape, closing a blind spot where an entire worktree-store category was invisible to `tm doctor`, `--dry-run`, and the automatic sweep.
- The `decommission` owner gate is forward-looking infrastructure: it closes a genuine cross-session authority gap (a rogue or buggy MCP-driven peer session could previously decommission any other session's worktree with no check at all) even though no current wire-facing caller populates a non-`None` identity yet.
- Purely additive: every new field is `#[serde(default)]`, every new function parameter has a safe default (`None`) at every existing call site, and the sentinel parser treats anything it doesn't recognize as "unknown," never as an error.

### Negative / Trade-offs

- **Post-creation setter race window.** `set_worktree_owner` runs after the record is persisted, not atomically with creation — a session that crashes between `create_with_id` and `set_worktree_owner` succeeding leaves that one record owner-unknown (indistinguishable from a legacy record). This is a narrow, best-effort window consistent with several other post-creation setters already in this codebase (`set_workspace_owned`, `set_source_id`); it does not regress safety (owner-unknown is still the conservative default), only occasionally under-attributes a very recently created session.
- **MCP caller identity is not yet wired.** The `caller` parameter exists end-to-end in `decommission`/`prune_managed`, but the MCP transport (`mcp_session::session_decommission`/`session_prune`) has no per-connection session identity to populate it with today, so it is always `None` at that boundary — the gate currently only fires in direct `SessionManager` API usage (and its own test suite). Closing this requires wiring `TM_MANAGED_SESSION_ID` (or an equivalent) through the MCP dispatch layer, tracked as follow-up work against #3649.
- **Legacy worktrees stay unowned forever** (by design — see "Zero migration" above) — an operator who wants old worktrees reclaimed still needs to run the existing manual `tm session prune-worktrees` sweep, exactly as before this ADR.

### Neutral / Follow-up work

- Wire an MCP-layer caller identity into `session_decommission`/`session_prune` so the owner gate is enforced for agent-to-agent MCP calls, not just direct API callers.
- Consider whether the harness `.claude/worktrees/` store should eventually share this ownership model (out of scope here — see above).

## Related Decisions

Vetted against prior ADRs on 2026-07-22:

- **ADR-0012 (Per-instance GUID and marker-file identity):** Consistent. This ADR follows the same "durable on-disk identity marker" pattern (a JSON-payload sentinel file) already established for per-instance daemon identity; no conflict.
- **ADR-0008 (Project-identity convention):** Consistent. Worktree paths continue to nest under the existing `<owner>/<repo>` project-identity convention; this ADR only adds an ownership annotation inside that existing layout, changing no path shape.
- **ADR-0016 (Orchestration Hierarchy):** Consistent. The `caller`-gated decommission is a session-level (not role-level) authority check; it is complementary to, not a replacement for, the PM/Assistant/Engineering-Lead role hierarchy that ADR-0016 defines — an operator (role-authorized) always passes `caller: None` and is never subject to this gate.

No conflicts identified.
