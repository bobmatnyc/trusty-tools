# trusty-mpm Architecture: Memory, Sessions, Search

This document describes three foundational design patterns shipped on trusty-mpm's
`main` branch that govern how the framework manages memory access, session isolation,
and code search. All three are active in every managed session; understanding their
trade-offs and failure modes is essential for building reliable tools and debugging
production behaviour.

## 1. Memory over MCP/JSON-RPC, never a guessed port

**The problem:** Early versions of trusty-mpm reached the trusty-memory daemon by
hard-coding a fixed port (e.g., `127.0.0.1:7070`) or building an ad-hoc REST path
(`/api/v1/palaces/{id}/drawers`). Because trusty-memory auto-port-walks from ~7070
to 7079 when the preferred port is in use, a hand-rolled fixed-port connection
silently failed even when the daemon was healthy — unless an operator manually
exported `TRUSTY_MEMORY_URL` to override it. Call sites were scattered: each one
reached memory differently, and the failure modes were opaque.

**The solution:** All trusty-mpm code that reaches trusty-memory now uses a single
shared helper in `trusty-common/src/mcp/memory_rpc.rs` (issue #2030):

1. **Discovery-based resolution:** [`trusty_common::read_daemon_addr("trusty-memory")`](../../../crates/trusty-common/src/daemon_addr.rs)
   resolves the daemon's actual bound address from its discovery file, eliminating
   port guessing entirely. This is the same mechanism trusty-search and trusty-analyze
   use.

2. **JSON-RPC over POST /rpc:** Once the address is known, trusty-mpm POSTs a
   JSON-RPC envelope (method, params, id) to `<base-url>/rpc` — the same dispatcher
   that the MCP stdio server and UDS transport share inside the daemon. This is the
   one "correct" RPC surface; REST paths specific to the monitor TUI or subprocess
   bridges are never used by the daemon.

3. **Shared callsites:** [`trusty_common::call_memory_tool`](../../../crates/trusty-common/src/mcp/memory_rpc.rs)
   wraps the discovery + RPC call so a session never guesses. The address can be
   overridden via `TRUSTY_MEMORY_URL` for CI/dev environments; env wins over
   discovery.

**Practical impact:**
- The daemon can rebind to a new port on startup, and trusty-mpm will find it.
- Teardown/respawn cycles work without manual intervention.
- Failure-to-reach is detectable (a `Err` from `call_memory_tool`, not a silent
  timeout or 404).

**See also:** [trusty-common `daemon_addr` module](../../../crates/trusty-common/src/daemon_addr.rs),
[issue #2030](https://github.com/bobmatnyc/trusty-tools/pull/2030).

---

## 2. Session↔Worktree 1:1 Model + Semantic Naming

**The problem:** Prior to issue #2032, each managed session was backed by a git
worktree with a UUID-based name (e.g., `.worktrees/fb2c8a12-4e9e-11ec.../`). The
session record held the UUID; the worktree directory was an implementation detail.
This meant:

- A human operator could not map a worktree on disk back to a running session without
  consulting the session database.
- Naming collisions between session IDs and worktree directories were impossible to
  predict or reason about.
- Documenting "the worktree is at `.worktrees/<that-uuid>`" leaked internal IDs into
  user-facing guidance.

**The solution:** As of issue #2032, each managed session gets a semantic tmux
session name first, and the worktree is named after it:

1. **Semantic tmux session name derivation:** The [`SessionManager::resolve_session_name`](../../../crates/trusty-mpm/src/session_manager/naming.rs)
   method applies a priority-ordered derivation:
   - Explicit `name_hint` (if provided) → `tm-<hint-slug>-NN`
   - GitHub-parseable `repo_url` → `tm-<repo-name>-NN`
   - `cwd` basename otherwise → `tm-<cwd-basename>-NN`
   
   The `-NN` suffix is a per-project serial (e.g., `-01`, `-02`) so concurrent
   sessions for the same project get distinguishable names. A candidate name is
   rejected if it collides with a live tmux session or (for in-project spawns) with
   an existing `.worktrees/<name>` directory or `session/<name>` branch.

2. **1:1 session↔worktree mapping:** The session's tmux name becomes the worktree
   directory name. For in-project spawns, the worktree lives at
   `<base>/.worktrees/<semantic-name>/`, branched as `session/<semantic-name>` off
   the main base clone.

3. **Decommission atomicity:** When a session is decommissioned, the worktree and
   its branch are removed together — one logical operation, one consistent state.

4. **UUID remains internal:** The session record still carries a UUID for internal
   routing and identity verification. The UUID is not exposed in user-facing paths
   or documentation.

**Practical impact:**
- A human operator can read a worktree path like `.worktrees/tm-trusty-tools-01/`
  and know immediately which session it belongs to, without consulting a database.
- Session names are stable across restarts (same derivation logic), so a session
  resumed from a previous state gets the same name.
- Guidance can reference semantic names (`"the session is in .worktrees/tm-foo-01"`)
  instead of UUIDs.

**See also:**
- [Session manager naming logic](../../../crates/trusty-mpm/src/session_manager/naming.rs)
  (issue #2032)
- [In-project spawn path](../../../crates/trusty-mpm/src/daemon/managed_routes/inproject.rs)
  (describes worktree creation and branch semantics)
- [DOC-26: trusty-mpm alpha-1 control plane](../../../docs/specs/trusty-mpm-alpha-1-control-plane.md)

---

## 3. Per-Worktree Search-Index Model

**The problem:** Code search is most useful when scoped to the project being worked
on, but trusty-search's default behaviour is to search across all indexes it knows
about. If a session ran a bare `search` query without pinning an index, the daemon
would need guidance (from the LLM or an explicit parameter) to pick the right one —
and it routinely picked the wrong one (often a persistent index from a previous
project, like `claude-mpm`). This created silent failures: a query appeared to
complete but returned results from the wrong codebase.

**The solution:** Every managed session gets a project-specific search index, and
that index ID is pinned into the session's `.mcp.json` stub (issue #1373):

1. **Index ID derivation:** Given a project's worktree path, [`trusty_common::derive_index_id`](../../../crates/trusty-common/src/index_id.rs)
   computes a stable, reproducible index ID. The ID is based on the project's
   directory name (leaf basename, normalized) so the same project always gets the
   same index ID across sessions and machines.

2. **Pinning in the MCP stub:** During session setup, [`trusty_search_mcp_value`](../../../crates/trusty-mpm/src/core/session_launch/search_index.rs)
   injects a pinned `.mcp.json` entry: `trusty-search serve --index <id>`. When the
   session calls any search tool (`search`, `grep`, `get_call_chain`, etc.), the
   daemon receives the explicit index ID and queries only that index — no ambiguity,
   no wrong-index surprises.

3. **Index lifecycle:** The lifecycle is tied to the session's worktree:
   - **Create:** When a session's worktree is provisioned, the daemon creates the
     index (if absent) and registers it with trusty-search via the `trusty-search`
     MCP tool [`index_status`](../../../crates/trusty-search/src/mcp_tools/index_status.rs).
     A best-effort reindex is triggered so the index is populated before the session
     starts real work.
   - **Watch:** A background file-watcher (part of the trusty-search daemon) keeps
     the index fresh as the session edits files.
   - **Decommission:** When the session is decommissioned (and its worktree removed),
     the index is marked for deletion. The daemon's garbage-collection sweep (triggered
     on daemon startup and periodically) removes orphaned indexes.

4. **Orphan handling:** If a session crashes and the worktree is lost, the index
   becomes orphaned. The GC sweep detects this by checking whether the index's
   recorded worktree path still exists on disk; if not, it removes the index. This
   prevents stale indexes from accumulating over time.

**Practical impact:**
- A session's `search foo` query always searches the right project, with no operator
  intervention.
- Sessions can run in parallel on the same or different projects; their indexes are
  independent.
- Index cleanup is automatic: decommission the session, and the index is eventually
  removed by GC.

**See also:**
- [Index ID derivation](../../../crates/trusty-common/src/index_id.rs)
- [Session launch: search-index registration](../../../crates/trusty-mpm/src/core/session_launch/search_index.rs)
- [Session manager: decommission & search GC](../../../crates/trusty-mpm/src/session_manager/decommission.rs),
  [`search_gc.rs`](../../../crates/trusty-mpm/src/session_manager/search_gc.rs)
- [issue #1373: pinned index IDs](https://github.com/bobmatnyc/trusty-tools/issues/1373)

---

## Integration: How These Three Work Together

When you run `tm run` to spawn a new managed session:

1. **Memory contact:** trusty-mpm reaches trusty-memory to provision a per-session
   memory palace using [`call_memory_tool`](../../../crates/trusty-common/src/mcp/memory_rpc.rs)
   — no port guessing, discovery-based resolution.

2. **Session + worktree naming:** The session's semantic name is derived via
   [`SessionManager::resolve_session_name`](../../../crates/trusty-mpm/src/session_manager/naming.rs),
   and the worktree is created at `.worktrees/<name>/` on branch `session/<name>`.

3. **Search index pinning:** The worktree's path is fed to [`derive_index_id`](../../../crates/trusty-common/src/index_id.rs),
   and the resulting index ID is pinned into the `.mcp.json` stub via
   [`trusty_search_mcp_value`](../../../crates/trusty-mpm/src/core/session_launch/search_index.rs).

4. **Cleanup:** On decommission, the worktree, branch, and search index are removed
   together in one logical operation, leaving the project clean.

---

## Reference Documentation

For deeper protocol details, implementation decisions, and operational guidance:

- [Three-Harness Architecture](../../../docs/architecture/harnesses.md) — the daemon,
  control-plane, and harness delegation graph
- [DOC-26: trusty-mpm alpha-1 control plane](../../../docs/specs/trusty-mpm-alpha-1-control-plane.md) —
  session lifecycle contract, wire protocol, failure semantics
- [Running MCP servers locally](../../../docs/reference/running-mcp-servers.md) —
  how to start trusty-memory and trusty-search for development
- [Environment variables reference](../../../docs/reference/environment-variables.md) —
  `TRUSTY_MEMORY_URL`, `TRUSTY_MPM_REPOS_ROOT`, index registration overrides

---

## Debugging & Verification

**Memory contact failing?**
- Check `TRUSTY_MEMORY_URL` env var; if unset, verify the daemon is running and
  `~/.trusty-mpm/daemon/trusty-memory.sock` exists (or the discovered address
  file in `~/.cache/trusty-tools/`).
- Review trusty-memory daemon logs for startup errors.

**Worktree not being created?**
- Verify the base clone exists at `~/trusty-mpm-projects/<owner>/<repo>/`.
- Check that the session name derivation succeeded (no collision with live
  tmux sessions or existing worktree dirs).
- Inspect the session record's UUID and worktree path; file a bug if they are
  inconsistent.

**Search query returning wrong results?**
- Verify the session's `.mcp.json` contains `"args": ["serve", "--index", "<id>"]`.
  If missing, the pinning failed; check session launch logs.
- Run `trusty-search index_status <id>` to confirm the index exists and has
  recent file metadata.
- If the index is stale, trigger a manual reindex via the daemon API.

---

## Design Rationale

All three patterns privilege **discoverability and stability** over performance or
flexibility:

- **Memory:** Hardcoded ports are fast but fragile; discovery is one extra resolve
  call, but the daemon can be restarted, reparented, or relocated without operator
  intervention.

- **Sessions:** Semantic names add a small derivation step per spawn, but human
  operators can now reason about the layout on disk without consulting the database.

- **Search:** Pinning each session to its index reduces the daemon's query fan-out
  (smaller result sets, faster queries), eliminates wrong-index surprises, and
  simplifies GC — one index per worktree, cleanup happens together.

Each pattern is independent but mutually reinforcing: memory enables the session
record; semantic naming enables the worktree; the worktree path enables index
derivation.
