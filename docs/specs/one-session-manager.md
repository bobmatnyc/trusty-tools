# DOC-33 — One Session Manager: consolidate trusty-agents' TmManager onto trusty-mpm

**Status:** Draft
**Subsystem:** trusty-mpm (canonical session manager) + trusty-agents (thin client)
**Owner:** Engineering (trusty-tools)
**Last-updated:** 2026-07-03
**Spec ID:** `SPEC-ONESM-01~draft` … `SPEC-ONESM-06~draft` (DOC-33)
**Builds on:** the "no fire-and-forget" tmux-lifecycle standard (EPIC #1452 — every
tmux session owned by a single Session with tracked state); the `tmpm-<slug>-<8hex>`
naming convention (PR #1791).
**Cross-ref:** `crates/trusty-mpm/src/session_manager/` (canonical),
`crates/trusty-mpm/src/core/names.rs` (`tmpm-` naming),
`crates/trusty-mpm/src/daemon/managed_routes/` (HTTP session API),
`crates/trusty-agents/src/tm/` (TmManager — the parallel manager to retire),
`crates/trusty-agents/src/tmux/` (its tmux orchestrator),
`crates/trusty-agents/src/repl/`, `crates/trusty-agents/src/tools/tm_tools/` (consumers).

---

## 1. Problem

There are **two session managers** in the workspace, each spawning and naming tmux
sessions independently:

| | trusty-mpm `session_manager/` | trusty-agents `src/tm/` (`TmManager`) |
|---|---|---|
| Role | **Canonical** — daemon-backed, single-owner Session, tracked lifecycle | Multi-adapter orchestration inside the `tagent` REPL |
| Naming | `tmpm-<slug>-<8hex>` (`core::names`) | `<project>-<harness>-<serial>` (e.g. `tm-writing-01`) |
| Lifecycle | reconcile/adopt/prune filter on the `tmpm-` prefix | own registry (`src/tm/registry.rs`), own `src/tmux/` orchestrator |

**Symptom (reported 2026-07-03):** `tm-writing-01` / `tm-claude-mpm-skills-01` sessions
appear in `tmux ls`. They do **not** carry the `tmpm-` prefix, so trusty-mpm's
reconcile/prune/adopt (which filter on `tmpm-`) never see them — orphaned sessions,
violating the "no fire-and-forget" standard. Two managers = two conventions = drift.

**Directive (Bob, 2026-07-03):** *"There should be one session manager."*

## 2. Decision

- **trusty-mpm's daemon is the SINGLE authority** for tmux session lifecycle
  (create, name, track, observe, stop, decommission) across the workspace.
- **trusty-agents' `TmManager` becomes a THIN CLIENT** of that daemon (chosen over
  "absorb + delete" to preserve the `tagent` REPL's multi-adapter orchestration UX
  and minimize blast radius). It keeps its adapter/harness-specific concerns (what to
  run inside a session, REPL commands, monitoring surface) but **stops owning tmux
  directly** — every spawn/track/stop delegates to the daemon, so sessions get
  `tmpm-` names and full lifecycle tracking.
- **Non-goal:** removing the `tagent` REPL or its adapter model. This unifies session
  *ownership*, not the orchestration UX above it.

## 3. Dependency direction (the enabling constraint)

trusty-agents **must not** depend on trusty-mpm (wrong direction / near-circular).
Both already depend on **`trusty-common`**. Therefore:
- The `tmpm-` naming (`trusty-mpm::core::names`: `PREFIX`, `name_from_uuid`,
  `name_from_dir`, `is_managed_name`) is **extracted to `trusty-common`** and both
  crates consume it. (SPEC-ONESM-01)
- trusty-agents talks to the daemon over its **HTTP API** (`/api/v1/sessions/managed`,
  `.../runtime-stop`, `.../decommission`, `GET .../managed`) — a client dependency,
  not a code dependency. (SPEC-ONESM-03)

## 4. Requirements

- **SPEC-ONESM-01 (shared naming):** Move the `tmpm-` naming helpers to `trusty-common`
  (e.g. `trusty_common::session_naming`); trusty-mpm re-exports for compatibility;
  trusty-agents adopts them. After this, trusty-agents-created sessions are named
  `tmpm-<slug>-<8hex>` — the reported symptom is fixed even before full delegation.
- **SPEC-ONESM-02 (single lifecycle authority):** No code path outside
  trusty-mpm's `session_manager` may call `tmux new-session`/`kill-session` for a
  managed harness session. trusty-agents' `src/tmux/` direct spawn/kill is removed or
  routed through the daemon.
- **SPEC-ONESM-03 (thin client):** `TmManager::{spawn,stop,list,observe}` call the
  daemon's session HTTP API. Adapter/harness type maps to the daemon's `runtime`
  (claude-code / tcode) or a new runtime variant if a harness has no equivalent.
- **SPEC-ONESM-04 (single registry):** trusty-agents' `src/tm/registry.rs` becomes a
  read-through view over the daemon's managed-session list, not an independent store,
  so there is one source of truth for session state.
- **SPEC-ONESM-05 (fail-soft):** When the daemon is unreachable, the REPL degrades
  gracefully (clear error, no silently-orphaned tmux) — never falls back to
  self-spawning an untracked `tm-*` session.
- **SPEC-ONESM-06 (no orphans):** After consolidation, `tmux ls` contains only
  `tmpm-`-prefixed managed sessions (plus explicitly-adopted externals); a conformance
  test asserts trusty-agents creates no non-`tmpm-` session.

## 5. Phased migration (each phase is an independently-shippable PR)

- **Phase 1 — shared naming (SPEC-ONESM-01).** Extract `tmpm-` naming to
  `trusty-common`; trusty-mpm re-exports; **trusty-agents' `next_session_name` /
  session creation adopt it** → sessions become `tmpm-<slug>-<8hex>`. Fixes the
  visible `tm-writing-01` symptom immediately; low-risk, no behavior change beyond
  names. *(This is the first implementation step — start here.)*
- **Phase 2 — delegate spawn/stop (SPEC-ONESM-03).** `TmManager` spawn/stop call the
  daemon HTTP API instead of `src/tmux/`. Map adapter → runtime.
- **Phase 3 — single registry (SPEC-ONESM-04).** `registry.rs` reads the daemon's
  managed list; drop the duplicate persistent store.
- **Phase 4 — remove the parallel orchestrator (SPEC-ONESM-02).** Delete/retire
  `src/tmux/` direct session control and the now-dead parts of `src/tm/`; add the
  no-orphans conformance test (SPEC-ONESM-06).

## 6. Open questions

1. **Adapter/harness → runtime mapping.** trusty-agents tracks `AdapterType::ClaudeMpm`
   (and others); the daemon spawns `claude-code`/`tcode`. Does every adapter map to an
   existing runtime, or does the daemon need a new runtime kind (e.g. `claude-mpm`)?
   Resolve before Phase 2.
2. **Registry migration.** Existing trusty-agents session records (its own store) —
   one-time import into the daemon's store, or drop on cutover?
3. **REPL monitor.** `TmMonitor` polls its own registry every 30s; repoint at the
   daemon's activity API or the SSE `/events` stream.
4. **`next_session_name` serial semantics** (`<project>-<harness>-<serial>`) are lost
   under `tmpm-<slug>-<8hex>`; confirm no REPL UX depends on the human-readable serial.
