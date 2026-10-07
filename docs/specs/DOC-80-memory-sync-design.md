---
spec_refs:
  - id: SPEC-MEMSYNC-REQ-03~draft
    path: docs/specs/DOC-79-memory-sync-requirements.md
    anchor: SPEC-MEMSYNC-REQ-03~draft
    note: content boundary (Addendum E); §5 schema enforces it
  - id: SPEC-MEMSYNC-REQ-05~draft
    path: docs/specs/DOC-79-memory-sync-requirements.md
    anchor: SPEC-MEMSYNC-REQ-05~draft
    note: sync-only connector, local recall
  - id: SPEC-MEMSYNC-REQ-12~draft
    path: docs/specs/DOC-79-memory-sync-requirements.md
    anchor: SPEC-MEMSYNC-REQ-12~draft
    note: process isolation, failure rules and freshness
  - id: SPEC-MEMSYNC-REQ-13~draft
    path: docs/specs/DOC-79-memory-sync-requirements.md
    anchor: SPEC-MEMSYNC-REQ-13~draft
    note: conformance cases; §12 gives the suite mechanics
  - id: SPEC-MEMSYNC-REQ-15~draft
    path: docs/specs/DOC-79-memory-sync-requirements.md
    anchor: SPEC-MEMSYNC-REQ-15~draft
    note: dream worker; §10 gives the design
---

# DOC-80 — Shared Engineering-Project Memory Sync: Design, API Proposal and Plan

**Status:** Draft (design proposal; no runtime implementation authorized)
**Spec ID:** `SPEC-MEMSYNC-01~draft` … `SPEC-MEMSYNC-11~draft` (DOC-80)
**Subsystem:** `trusty-memory` (sync connector and dream worker child processes, daemon RPCs, status); `trusty-common` (`memory_core` record model and dream passes, changed only through the Architect); remote memory-store API (see [ADR-0069](../adr/0069-remote-memory-store-api-spec-location.md)); reference endpoint (see [ADR-0070](../adr/0070-memory-store-reference-implementation-repository.md))
**Owner:** Bob Matsuoka (rulings); runtime owner assigned by the Architect (implementation)
**Last-updated:** 2026-10-04 (r2: review findings, owner decisions on OQ-1 to OQ-4, dream worker)
**Requirements:** [DOC-79](./DOC-79-memory-sync-requirements.md). Requirement IDs (`R-…`) and conformance cases (`C-…`) below refer to it.
**Decisions:** [ADR-0068](../adr/0068-memory-sync-and-dream-run-as-daemon-supervised-child-processes.md), [ADR-0069](../adr/0069-remote-memory-store-api-spec-location.md), [ADR-0070](../adr/0070-memory-store-reference-implementation-repository.md), all Proposed.
**DOC-N claim:** see DOC-79's header.

Marking rule: **VERIFIED** means the cited `path:line` was read at `origin/main` e3ff4c7358 on 2026-10-04. **PROPOSED** means design, not code. Planned files and modules are written without backticks because they do not exist.

---

## 1. Summary

- Two child processes of the `trusty-memory` daemon, under one supervision model: a sync connector that moves memory records between the local store and remote endpoints, and a dream worker that runs the consolidation passes now run in-process. Neither answers recall (R-SYNC-1, R-PROC-1, R-DREAM-1).
- Each child reaches the store only through authenticated daemon RPCs on the existing Unix socket. The daemon applies every change atomically with its journal entry, so a child crash never leaves a partial change (R-SYNC-5, R-SYNC-8, R-DREAM-3).
- Records travel in a closed-schema envelope with project id, memory id, revision id, parents, decision state, canonical artifact links and provenance roles. Every nested object is closed; ingestion rejects work-item fields and work-tracking syntax (R-CONT-1, R-CONT-10).
- The API is HTTP+JSON with a per-project change feed, idempotent batch publish, batch get and tombstones. Authentication is provider-neutral at the API; GitHub App is the recommended first identity provider, subject to Bob's decision (OQ-5, open).
- Decided 2026-10-04 (Bob): OQ-1 child process, OQ-2 committed project UUID plus bindings (with an explicit local bind), OQ-3 sync identity out of `palace.json`, OQ-4 typed drawer fields after M2. §13 holds the decisions and the remaining open questions.

## 2. Baseline: what exists today

### 2.1 Process model

| Fact | Status |
|---|---|
| The dream cycle is an in-process tokio task per palace, not a separate process. `startup_tasks.rs` calls `spawn_background_maintenance`; the scheduler calls `start_with_shutdown`; that function calls `tokio::spawn`. | VERIFIED `crates/trusty-memory/src/startup_tasks.rs:135`, `crates/trusty-memory/src/dream_scheduler.rs:122`, `crates/trusty-common/src/memory_core/dream/dreamer.rs:145` |
| Dream scheduling can be disabled by environment variable. | VERIFIED `crates/trusty-memory/src/dream_scheduler.rs:85` |
| Maintenance (dream passes, TTL purge) runs only in the process that holds a per-data-root lease; user writes are not elected. | VERIFIED `crates/trusty-common/src/memory_core/maintenance_lease.rs:1-8`, `crates/trusty-common/src/memory_core/registry.rs:320`, `crates/trusty-common/src/memory_core/dream/dreamer.rs:167` |
| The daemon itself is supervised by launchd with `KeepAlive::OnSuccess`. | VERIFIED `crates/trusty-memory/src/commands/service.rs:167` |
| A reusable child-process supervisor exists: `EmbedderSupervisor` restarts on non-zero exit with exponential backoff, default 5 restarts and a 60 s cap, and exposes `has_given_up`. It is not used by trusty-memory today. | VERIFIED `crates/trusty-common/src/embedder_client/supervisor.rs:98`, `:228`, `:355`; `crates/trusty-common/src/embedder_client/supervisor_config.rs:84-85` |
| A child can arm exit-on-parent-death. | VERIFIED `crates/trusty-common/src/parent_death.rs` (module) |
| Counter-precedent: BM25 once ran as a supervised per-palace subprocess and was collapsed in-process ([#5329](https://github.com/bobmatnyc/trusty-tools/issues/5329)). | VERIFIED `crates/trusty-memory/src/bm25_lane.rs:3-4` |

### 2.2 Write boundary and storage

| Fact | Status |
|---|---|
| The daemon serves one Unix socket, JSON-RPC, and no HTTP listener (ADR-0032). `serve --http` now selects daemon mode on that socket. | VERIFIED `crates/trusty-memory/src/transport/uds.rs:1-7`, `:403-405`; `crates/trusty-memory/src/main.rs:937-945` |
| A Rust client for that socket exists. | VERIFIED `crates/trusty-common/src/memory_rpc.rs:234`, `:250` |
| redb takes an exclusive `flock` on each database file, and an open with `Writer` intent returns an error while another process holds the file. A second process therefore cannot write a palace store the daemon holds. | VERIFIED `crates/trusty-common/src/memory_core/store/concurrent_open.rs:3`, `:63-67` |
| `memory_remember` accepts palace, text, room, wing, tags, force, allow_secret_like, context, fact_key, expires_at, cwd, workstream. It accepts no id and no `created_at`. | VERIFIED `crates/trusty-memory/src/tools/definitions.rs:111-132` |
| Every new drawer gets a fresh UUIDv4. | VERIFIED `crates/trusty-common/src/memory_core/palace.rs:316` |
| Writes take a per-palace write mutex; the vector upsert runs inside the write pipeline. | VERIFIED `crates/trusty-common/src/memory_core/retrieval/handle.rs:168`, `crates/trusty-common/src/memory_core/retrieval/write_pipeline.rs:121`, `:336` |
| The BM25 lane is fed through a bounded channel that drops when full; a coverage-repair sweep exists. | VERIFIED `crates/trusty-memory/src/tools/helpers.rs:621`, `crates/trusty-memory/src/bm25_backfill.rs:11-14`, `crates/trusty-memory/src/startup_tasks.rs:147` |

### 2.3 Record model and deletion

| Fact | Status |
|---|---|
| `Palace` is `{id, name, description, created_at, data_dir}`. ADR-0051's `owner`/`project` fields are absent. | VERIFIED `crates/trusty-common/src/memory_core/palace.rs:44-50` |
| `Drawer` holds id, room id, content, importance, source file, `created_at`, tags, access stats, type, `expires_at`, `completed_at`, `fact_key` and a content hash. It has no revision id, no tombstone, no author and no machine field. | VERIFIED `crates/trusty-common/src/memory_core/palace.rs:219-301` |
| Drawer types include `UserFact`, `SessionEvent`, `AgentNote` and `Task`. | VERIFIED `crates/trusty-common/src/memory_core/palace.rs:124-148` |
| Creator attribution is ordinary tags (`creator:client=` and siblings), including a `creator:cwd=` tag that carries a local path. | VERIFIED `crates/trusty-memory/src/attribution.rs:40` |
| `PalaceHandle::forget` is a hard delete. User forgets never reach the maintenance deletion journal. | VERIFIED `crates/trusty-common/src/memory_core/retrieval/handle.rs:870`, `crates/trusty-common/src/memory_core/maintenance_log.rs:15-16` |
| Supersession exists as a `superseded_by` KG triple. | VERIFIED `crates/trusty-common/src/memory_core/share/supersede.rs:41` |
| KG triples carry `valid_from`, `valid_to` and `provenance`; drawers do not. | VERIFIED `crates/trusty-common/src/memory_core/store/kg/types.rs:42-44` |

### 2.4 Project identity

| Fact | Status |
|---|---|
| Palace resolution order: `TRUSTY_MEMORY_PALACE`, then the committed pin file, then the git `owner/repo` slug, then the parent-dir slug. | VERIFIED `crates/trusty-common/src/palace_resolve.rs:146-155`, `:398` |
| This repository pins `palace: trusty-tools`, overriding the git-derived `bobmatnyc-trusty-tools`. Two machines without the pin would derive different ids. | VERIFIED `.trusty-tools/trusty-memory.yaml` (line 7) |
| No palace GUID exists. The GUID migration ([#1191](https://github.com/bobmatnyc/trusty-tools/issues/1191)) was folded into [#1681](https://github.com/bobmatnyc/trusty-tools/issues/1681), now closed. | VERIFIED by the struct above and the issue states, read 2026-10-04 |
| `RepoIdentity` (`GitHub(owner/repo)` or root-commit `ContentHash`) exists in trusty-common and is used by trusty-search, not trusty-memory. | VERIFIED `crates/trusty-common/src/repo_identity.rs:61-63` |

### 2.5 Secret gate

| Fact | Status |
|---|---|
| The write pipeline runs `check_secret` even under `force`, unless `allow_secret_like` is set. | VERIFIED `crates/trusty-common/src/memory_core/retrieval/write_pipeline.rs:253` |
| The share import path runs no secret screen; its own doc comment says nothing screens content on either side. | VERIFIED `crates/trusty-common/src/memory_core/share/import.rs:406`, `:418` |

### 2.6 Existing sync-like code

| Item | State | Use for this design |
|---|---|---|
| `memory_core::share` ([#5902](https://github.com/bobmatnyc/trusty-tools/issues/5902)): JSONL `SharedMemoryRecord`, content-hash identity, idempotent import, earliest `created_at` wins. Excludes expired and Tier C drawers. | VERIFIED merged and unwired: `crates/trusty-common/src/memory_core/share/record.rs:66-83`, `crates/trusty-common/src/memory_core/share/export.rs:62` | Prior art for hashing and idempotent import. Lacks project id, memory id across edits, deletes, decision state, refs and a secret gate. |
| ADR-0062 session refs: per-writer append-only git refs with a pre-push credential scan. | VERIFIED implemented: `crates/trusty-mpm/src/core/session_ref_publish.rs`, `crates/trusty-common/src/catchup/session_refs.rs` | Precedent for per-writer streams and a fail-closed scan before data leaves the machine. |
| DOC-56 agent-config sync: block-never-redact secret gate, no auto-resolve. | Draft, no code (`docs/specs/trusty-agents-agents-sync.md`) | Policy precedent. |
| Residency pull ticker with a freshness state machine. | VERIFIED `crates/trusty-common/src/residency.rs:1-6` | Pattern for freshness status. |

### 2.7 Status surfaces

| Fact | Status |
|---|---|
| `palace_info` returns additive JSON including `last_used_unix`. | VERIFIED `crates/trusty-memory/src/tools/palace_ops.rs:229`, `:265` |
| `memory.dream_status` and `memory.palace_dream_status` are the per-worker status precedent. | VERIFIED `crates/trusty-memory/src/transport/methods/kg.rs:312-321` |
| Per-palace sidecar files avoid `palace.json`, which is rewritten wholesale. | VERIFIED `crates/trusty-memory/src/palace_last_used.rs:10-12` |

### 2.8 The dream cycle today (state read and written)

The dream worker design (§10) moves this code path out of the daemon. Each row is what the in-process cycle reads or writes today.

| Fact | Status |
|---|---|
| The scheduler builds one `Dreamer` per palace and spawns one loop each; each loop checks idleness and the maintenance lease every tick, then calls `dream_cycle` on a resident handle. | VERIFIED `crates/trusty-memory/src/dream_scheduler.rs:81`, `:115`, `:122`; `crates/trusty-common/src/memory_core/dream/dreamer.rs:138-180` |
| A cycle takes a process-wide concurrency permit (`TRUSTY_DREAM_MAX_CONCURRENT`) and sets the palace's `is_compacting` flag for its whole duration. | VERIFIED `crates/trusty-common/src/memory_core/dream/dreamer.rs:214`, `:223`; `crates/trusty-common/src/memory_core/dream/concurrency.rs:40`, `:179` |
| Passes in order: content prune, dedup, importance prune, vector compaction, closet refresh, optional LLM semantic consolidation, L1 flush, fading detection, optional KG compaction, then `dream_stats.json`. | VERIFIED `crates/trusty-common/src/memory_core/dream/dreamer.rs:237`, `:243`, `:246`, `:249`, `:252`, `:256`, `:265`, `:276`, `:323`, `:345-351` |
| Removals go through `forget_for_maintenance`, which writes the maintenance deletion journal. | VERIFIED `crates/trusty-common/src/memory_core/dream/cycle.rs:101`, `:385-389`, `:445`; `crates/trusty-common/src/memory_core/maintenance_log.rs:295` |
| Dedup merges by rewriting the survivor's content in the in-memory drawer table only; `flush` saves only the L1 cache and identity. Merged text is lost on restart ([#9172](https://github.com/bobmatnyc/trusty-tools/issues/9172)). | VERIFIED `crates/trusty-common/src/memory_core/dream/helpers.rs:125-148`, `crates/trusty-common/src/memory_core/retrieval/handle.rs:571-581` |
| Dedup picks the survivor by importance alone. | VERIFIED `crates/trusty-common/src/memory_core/dream/cycle.rs:375-379` |
| Semantic consolidation writes canonical drawers with `handle.remember` and records `superseded_by` provenance in the KG. | VERIFIED `crates/trusty-common/src/memory_core/dream/semantic.rs:131`, `:185` |
| `PalaceHandle::touch` records user access and is suppressed while `is_compacting` is set. The scheduler's idle check reads `Dreamer::is_idle`; no production caller of `Dreamer::touch` was found (grep, 2026-10-04; runtime owner to confirm). | VERIFIED `crates/trusty-common/src/memory_core/retrieval/handle.rs:331`; `crates/trusty-common/src/memory_core/dream/dreamer.rs:92`, `:97` |
| Manual triggers run in the daemon and refuse without the lease: `memory.dream_run` and the `dream_consolidate_room` tool. | VERIFIED `crates/trusty-memory/src/transport/uds.rs:320`, `crates/trusty-memory/src/service/core_kg.rs:423-427`; `crates/trusty-memory/src/tools/dream_ops.rs:52-56` |
| Open defects in this path: [#9172](https://github.com/bobmatnyc/trusty-tools/issues/9172) (merge persistence, survivor choice, unjournaled survivor removal), [#9173](https://github.com/bobmatnyc/trusty-tools/issues/9173) (scheduler loop count, palace not dreaming), [#8729](https://github.com/bobmatnyc/trusty-tools/issues/8729) (drawers lost with no log trail). | VERIFIED issue states, read 2026-10-04 |

## 3. Architecture {#SPEC-MEMSYNC-01~draft}

All of this section is PROPOSED.

```text
 machine M1                                              remote
 ┌──────────────────────────────────────────────┐
 │ trusty-memory daemon (launchd)               │
 │  recall ── local indexes (unchanged)          │
 │  write path ──► per-palace sync outbox        │
 │  sync RPCs:  outbox_read/_ack, ingest, report │
 │  dream RPCs: next, snapshot, neighbours,      │
 │              apply, maintain, report          │
 │  child supervisor ─┬─spawns─┐                 │
 │                    │        ▼  (UDS + secret) │
 │                    │  trusty-memory sync-connector ├──HTTPS──► endpoints
 │                    ▼                          │
 │        trusty-memory dream-worker ────────────┼──HTTPS──► inference backend (optional)
 └──────────────────────────────────────────────┘
```

### 3.1 One supervision model for both children

Decision: [ADR-0068](../adr/0068-memory-sync-and-dream-run-as-daemon-supervised-child-processes.md) (OQ-1 decided 2026-10-04).

- **Binary.** Each child is a hidden subcommand of the existing `trusty-memory` binary (proposed `sync-connector` and `dream-worker`). No new binary, so ADR-0043's cargo-bin policy is unchanged.
- **Spawn.** Only the daemon that holds the maintenance lease spawns children: the connector when at least one project has a sync binding, the dream worker unless `TRUSTY_DREAM_DISABLED` is set. A daemon that loses or never holds the lease spawns neither. Each child also takes a lock file in its state directory, so one data root has at most one of each.
- **Authentication.** At spawn the daemon creates a random secret per child and passes it on an inherited file descriptor, not in argv or the environment. Every sync or dream method requires it, and the daemon accepts each method family only from the matching child (R-SYNC-8, R-PROC-5).
- **Restart.** One supervisor instance per child, in the `EmbedderSupervisor` shape: exponential backoff from 1 s to a 60 s cap; give-up after 5 restarts within a 10-minute window; give-up shown as `given_up` in status with the last exit reason. Unlike `EmbedderSupervisor`, any exit the daemon did not request counts as a failure, including exit code 0, because both children are meant to run until stopped. A daemon restart, or an operator command (proposed `trusty-memory child restart <name>`), clears `given_up`.
- **Parent death.** Each child arms parent-death and exits when the daemon dies; the next daemon spawns fresh children.
- **Shutdown.** The daemon stops handing out work, sends SIGTERM, waits a grace period (proposed 10 s), then SIGKILL. Every store change is applied by the daemon atomically (§6.4, §10.4), so a kill at any point leaves no partial change.
- **Direction.** The daemon never calls a child. Children poll; a later wake hint is optional.
- **State directories.** Each child owns a state directory beside, not inside, the palace directories. The connector keeps endpoint configuration, cursors, retry and dead-letter queues and credential references there (keychain item names, never token values). The dream worker keeps nothing durable there; durable dream state lives in the daemon.
- **Failure isolation.** A child that is down, crashing or given up never blocks recall, remember or forget, and never freezes the daemon (R-PROC-3, R-DREAM-5). Its status reports the state.

### 3.2 Outbound flow

1. A local write, revision, retraction or forget of a sync-eligible record appends an entry to that palace's sync outbox in the same storage transaction as the drawer change. If the append fails, the change fails (R-FAIL-1). This also closes the gap that a user forget leaves no durable trace today (§2.3). A forget of an imported record writes a suppression marker instead and appends nothing (R-CHG-7).
2. The connector reads the outbox with a cursor, builds envelopes (§5), and runs the outbound secret gate (R-SAFE-1). A blocked entry is held with its descendant revisions (R-SAFE-4).
3. It publishes batches to each endpoint the project is bound to and acks an entry only on a final outcome. Retryable outcomes stay pending; final rejections become dead letters (R-FAIL-2, §6.4).
4. A periodic reconciliation scan compares eligible drawer ids with outbox and published state and reports any gap in status (R-FAIL-1).

### 3.3 Inbound flow

1. The connector pulls each endpoint's per-project change feed from its cursor. A `cursor_expired` answer triggers a full re-list and reconcile (R-CHG-8, §7.4).
2. Events with an unknown `schema_version` or `kind` are parked and hold the cursor (R-FAIL-3). Events the connector cannot authorize to the bound project are dropped and counted by reason.
3. The connector calls `memory.sync_ingest` with a batch. The daemon validates and applies each record (§6) and returns a per-record outcome. The connector advances its cursor only past events whose outcome is final and durable (R-FAIL-4).

### 3.4 Dreaming and sync

- The dream worker may nominate local records for eligibility review, and may consolidate local records into a new local revision whose envelope lists `derived_from`. It never widens eligibility; the daemon refuses any dream action that would (R-ELIG-3, R-DREAM-6). A consolidation is eligible only if every source is eligible for the same project (R-ELIG-5).
- The dream worker does not dedup, prune or rewrite imported records. Those are immutable foreign revisions; a duplicate is linked, not merged (R-DREAM-7).
- Imported records never enter the outbox unless a resharing policy says so (R-CHG-3).
- Each child is refused the other's methods (R-PROC-5).

## 4. Crate and repository responsibility map {#SPEC-MEMSYNC-02~draft}

"trusty-tools" is the repository. `trusty-memory`, `trusty-common` and `trusty-mpm` are crates in it.

| Piece | Home | Status | Notes |
|---|---|---|---|
| Connector and dream worker subcommands, child supervisor, sync and dream RPCs, status | `trusty-memory` crate | PROPOSED, per Bob 22:29Z and 2026-10-04 | [ADR-0068](../adr/0068-memory-sync-and-dream-run-as-daemon-supervised-child-processes.md) |
| Record fields for decision state, canonical refs, claim kind, expression, memory id and provenance | `trusty-common` `memory_core` | PROPOSED; typed drawer fields (OQ-4 decided), added after M2 | Requested through the Architect (§11) |
| Dream passes split into decide (worker) and apply (daemon) | `trusty-common` `memory_core::dream` | PROPOSED | After the [#9172](https://github.com/bobmatnyc/trusty-tools/issues/9172) and [#9173](https://github.com/bobmatnyc/trusty-tools/issues/9173) fixes; Architect assigns |
| Normative API text | `docs/specs/` (this DOC, later its own DOC when stable) | PROPOSED | [ADR-0069](../adr/0069-remote-memory-store-api-spec-location.md) |
| Wire types and machine-readable schema (OpenAPI 3.x, JSON Schema) | A small new library crate in trusty-tools (working name trusty-memory-sync-api), not `trusty-mpm` | PROPOSED; Bob suggested `trusty-mpm` as an option | [ADR-0069](../adr/0069-remote-memory-store-api-spec-location.md) compares both |
| Conformance suite and in-process fake endpoint (multi-instance) | trusty-tools, beside the wire-types crate | PROPOSED | Lets CI run without another repository |
| Deployable reference endpoint | Separate repository, created later and only on Bob's GO | PROPOSED, per Bob's suggestion | [ADR-0070](../adr/0070-memory-store-reference-implementation-repository.md) |
| Freshness and child-health display | `trusty-console` reads the new status through `crates/trusty-console/src/memory_uds` | PROPOSED, later stage | Display only |
| Harness integration (who shares, session attribution) | `trusty-mpm` | PROPOSED, later stage | Supplies principal and session ids to the connector configuration; holds no memory data |

## 5. Record envelope {#SPEC-MEMSYNC-03~draft}

All PROPOSED. Every object in the envelope, top-level and nested, is closed: an unknown member is a validation error at local ingest and at the endpoint (R-CONT-10, C-32). DOC-79 §3.1 is the single list of work-item fields that never appear, at any depth.

### 5.1 Identity and lineage

| Field | Required | Meaning |
|---|---|---|
| `schema_version` | yes | Envelope version, negotiated (§7.1). An unknown version is parked, not dropped (R-FAIL-3). |
| `project_id` | yes | Portable project identity: the project UUID from the pin file, after an explicit local bind (§5.6). Never a palace id or a path. |
| `memory_id` | yes | Minted once by the originator (UUIDv7); stable across revisions, endpoints and machines. |
| `revision_id` | yes | SHA-256 over the canonical JSON of the revision's content fields (§5.2 and §5.3); transport facts are excluded. Local ingest recomputes it and rejects a mismatch (R-ID-7). |
| `parents` | yes | Revision ids of the same memory that this revision follows; empty for the first revision. `parents` is lineage within one memory only. Two heads for one memory form a conflict set. |
| `kind` | yes | `revision`, `retraction` or `tombstone`. |

### 5.2 Content and decision state (Addendum E)

| Field | Required | Meaning |
|---|---|---|
| `decision_state` | yes | `tentative`, `decided` or `superseded` (R-CONT-4). It is the state of the memory as of this revision. |
| `superseded_by` | iff `superseded` | `{memory_id}` of a different memory, or one `canonical_refs`-shaped locator. Supersession across memories or into an artifact; never a revision of the same memory (that is `parents`) (R-CONT-5). |
| `claim_kind` | yes | `intent`, `rationale`, `tradeoff`, `open_question`, `decision`, `observation`, `hypothesis` or `preference`. |
| `expression` | yes | `user_expressed`, `assistant_synthesis` or `agent_observation` (R-CONT-7). |
| `content` | yes, except tombstone | The memory text, at most 8 KiB UTF-8. Content rules in §5.7. |
| `canonical_refs` | yes, may be empty | At most 32 entries, each closed: `{kind, uri, pinned, relation}`. `kind`: `issue`, `pull_request`, `spec_section`, `requirement`, `adr`, `commit`, `doc`. `uri`: `https://` URL or `gh:<owner_id>/<repo_id>#<n>` form, at most 512 bytes. `pinned`: a 40- or 64-hex commit id or a spec revision of the form `<DOC-N>@<git sha>`, at most 80 bytes. `relation`: `discusses`, `motivates`, `decision_recorded_in`, `implemented_by`, `superseded_into`. A ref holds a locator only, never artifact text (R-CONT-3). |
| `record_gap` | no | `no_artifact_yet` on a `decided` record whose decision has no canonical home yet (OQ-8, open). |
| `derived_from` | no | At most 64 `{memory_id, revision_id}` pairs a synthesis or consolidation came from. |
| `applicability` | no | Closed object: `platforms` (list of `macos`, `linux`, `windows`), `versions` (list of `{component, range}` with a semver range string, at most 16), `conditions` (list of strings of at most 200 bytes, at most 8). |
| `tags` | no | At most 16, each matching one allow-listed prefix: `topic:`, `area:`, `lang:`, `component:`, followed by `[a-z0-9._-]{1,64}`. Anything else is stripped before publish (`creator:cwd=`, `ws:` and path-bearing tags) or, at ingest, rejected (`status:`, `priority:`, any §3.1 name) (R-PROJ-5, R-CONT-1). |

### 5.3 Provenance (W3C PROV-O vocabulary, used as JSON field names)

| Field | Required | Meaning |
|---|---|---|
| `author` | yes | Closed `{principal_id, principal_kind: person or workload, on_behalf_of}`. Bound by authentication at publish (R-ID-3). |
| `editor` | no | Same shape; the principal who produced this revision if not the author. |
| `created_at` | yes | Author time of the first revision. Informative only; never used for ordering or authority (R-CONF-2). |
| `observed_at`, `valid_from`, `valid_to` | no | When the fact was observed and the period it claims to hold. |

Transport facts, outside the hashed body: `publisher` (connector instance), `host_endpoint`, `attested_by` (endpoint id that bound the author), `received_at`, `replicated_via`. Principal ids are provider-qualified and stable, for example `github:user:<numeric id>` (R-ID-5).

### 5.4 Allowed combinations

`claim_kind` × `decision_state` (R-CONT-12):

| `claim_kind` | `tentative` | `decided` | `superseded` |
|---|---|---|---|
| `decision`, `intent`, `rationale`, `tradeoff`, `preference` | yes | yes | yes |
| `open_question` | yes | no (an answer is a new `decision` memory that supersedes it) | yes |
| `observation`, `hypothesis` | yes | no | yes |

`expression` constraints: `agent_observation` is never `decided` (R-AUTH-12). `assistant_synthesis` is `decided` only when every `derived_from` source is `decided`.

### 5.5 Decision-state transitions

A transition is from the current head of a memory to a new revision whose `parents` name that head (R-CONT-12, C-07, C-34).

| From head | To `tentative` | To `decided` | To `superseded` | `retraction` | `tombstone` |
|---|---|---|---|---|---|
| (none, first revision) | yes | yes, if allowed by §5.4 | no | no | no |
| `tentative` | yes (refine) | yes (promote) | yes | yes | yes |
| `decided` | yes (reopen) | yes (refine) | yes | yes | yes |
| `superseded` | no | no | no | yes | yes |

A `superseded` head is terminal for content. Re-asserting a superseded claim takes a new memory. Older revisions of a memory are history, not current, whatever state they recorded; recall uses only heads.

### 5.6 Project binding (OQ-2 decided, with safeguard)

Decided 2026-10-04 (Bob): project identity is a project UUID committed in `.trusty-tools/trusty-memory.yaml` (schema version bump) plus per-provider bindings. Safeguard from review r1 (R-PROJ-6):

1. The pin file's UUID is a candidate only. Cloning a repository never starts sync.
2. The user binds explicitly: project UUID, local palace and endpoint (proposed `trusty-memory sync bind`). The bind is stored in the connector's state directory, not in `palace.json` (OQ-3 decided).
3. Before the bind succeeds, and at every connector start, the connector reads `GET /v1/projects/{project_id}` and requires its repository binding (for example `github:repo:<numeric id>`) to match the repository the checkout's remote resolves to. A mismatch refuses the bind or stops sync for that project and shows the reason in status.

### 5.7 Content rules (R-CONT-1)

Endpoint-conformant checks, run at local ingest and at every endpoint: no §3.1 field at any depth; no content line beginning with a §3.1 name followed by `:` (case-insensitive, after optional leading `-`, `*` or `#`); no Markdown task list of two or more items.

Client-only check, run by the publishing connector: content must not share a verbatim run of 200 or more characters, after whitespace normalization, with the body of a linked artifact the client can fetch. An unfetchable artifact skips the check and records that it was skipped. Endpoints are not required to fetch artifacts. C-09 remains the outcome test.

### 5.8 Mapping from the local drawer

| Local | Envelope | Rule |
|---|---|---|
| `UserFact`, `AgentNote` drawers | `revision` | Eligible only when classified (`claim_kind`, `expression`, `decision_state` all set by the user or a user-enabled classifier) and policy allows. An unclassified drawer is never eligible; no default sets `decided` (R-CONT-11). |
| Any other or unknown drawer type | none | Never synced. |
| `Task` drawers | none | Never synced: tasks are work tracking (R-CONT-1). |
| `SessionEvent` drawers | none | Never synced: machine-local and short-lived. |
| Commit drawers | none | Link the commit as a canonical ref instead. |
| Tier C (`fact_key`) drawers | none in v1 | Matches the existing export rule (§2.6); revisit in OQ-9 (open). |
| `content`, `created_at` | `content`, `created_at` | Copied after the §5.7 checks. |
| `id` (UUIDv4) | not sent | A daemon-side map links `memory_id` to the local drawer id; sync never changes `Drawer.id` semantics. |
| `superseded_by` KG triple | `superseded_by` | Translated both ways when it links two different memories. |

## 6. Ingestion boundary and consistency model {#SPEC-MEMSYNC-04~draft}

All PROPOSED unless marked.

### 6.1 Why a new RPC

The connector cannot open a palace for writing (VERIFIED §2.2). `memory_remember` cannot carry a memory id or an author time and stamps the caller's own attribution (VERIFIED §2.2, §2.3). `share::import` preserves `created_at` but runs in-process only and has no secret gate (VERIFIED §2.5, §2.6). So inbound records need a dedicated daemon method.

### 6.2 Daemon methods (sync family)

Every method in this table requires the connector's spawn secret (§3.1); any other caller is refused (R-SYNC-8, C-30).

| Method | Caller | Behavior |
|---|---|---|
| `memory.sync_ingest` | connector | Batch of envelopes for one project. Per record: validate the closed schema and §5.4/§5.5 rules, recompute `revision_id`, check that `project_id` is bound to the target palace, check §5.7 content rules, run the local secret gate (R-SAFE-2), dedupe by `(memory_id, revision_id)`, honour suppression markers (R-CHG-7), apply under the per-palace write lock, make the record durable in every index lane or enqueue a durable backfill entry, record origin as imported with the source endpoint and `attested_by`, then return `applied`, `duplicate`, `conflict_recorded`, `parked` or `rejected` with a problem detail. |
| `memory.sync_outbox_read` / `memory.sync_outbox_ack` | connector | Cursor read of a palace's outbox; ack takes a final outcome (`published`, `dead_letter`) per entry. Held entries (R-SAFE-4) are returned with their hold reason and cannot be acked as published. |
| `memory.sync_report` | connector | Per-project, per-endpoint counters: last success, pending, held, dead letters, parked, drops by reason, conflicts. The daemon stamps receipt time; staleness is derived by the daemon (§6.4 F9). |
| `palace_info` `sync` key, `console_metrics` per-palace keys, child health | any reader | Additive status, following the additive pattern already used (§2.7). |

### 6.3 Consistency guarantees

| Scope | Guarantee |
|---|---|
| One record, local | Atomic visibility: recall sees an imported record in every lane it is indexed for, or does not see it. `applied` is returned only when that holds or a durable backfill entry exists for a lagging lane (R-FAIL-4). Recall tolerates a lane that is still backfilling. |
| Local writes | Unchanged: read-your-writes on the machine that wrote. The outbox entry commits with the drawer change (R-FAIL-1). |
| One memory, across replicas | Causal order through `parents`. Heads are revisions with no child. One head is current. Several heads form a conflict set: all are kept and recalled with a conflict marker; a `decided` head outranks a `tentative` one (R-CONT-6); otherwise none is labelled current. |
| One project, one endpoint | The change feed is append-only with a monotonic opaque cursor; delivery is at-least-once; apply is idempotent, so the effect is once-only (R-CHG-2). |
| Across endpoints | No ordering guarantee. The client merges by memory and revision id. |
| Clocks | Timestamps never decide order or authority (R-CONF-2). |
| Retraction | Marks a revision invalid; it stays for audit and leaves recall as current. A later revision may re-assert unless the head is `superseded` (§5.5). |
| Delete | Terminal for that memory. Every replica purges the body everywhere listed in §6.4 F11 and keeps a tombstone (ids, issuer, time, reason) for the published retention window (R-CHG-5). Who may delete follows OQ-7 (open). |
| Revocation | A 403 or an `access.revoked` event stops push and pull for that project at once, marks the local partition `access_revoked` and applies the cache policy (default quarantine pending OQ-6). A 401 refreshes the token and retries; it never triggers revocation (R-AUTH-10). Independently, the daemon quarantines a project's imports when its access lease lapses (R-AUTH-9). Content already read cannot be unseen; the spec says so (R-AUTH-7). |
| Endpoint unavailable | Freshness lags; recall and the daemon are unaffected (R-PROC-3). The connector backs off per endpoint. |

### 6.4 Fail-closed rules

Each row closes one fail-open path from review r1 (F1 to F12) and names the requirement and the error-arm case.

| Id | Failure | Rule | Requirement / case |
|---|---|---|---|
| F1 | Outbox append fails or is skipped after a drawer write, so a record is never published, or a forget never reaches the remote. | The append commits in the same storage transaction as the drawer change; a failed append fails the change. A reconciliation scan compares eligible drawer ids with outbox and published state and reports gaps. | R-FAIL-1 / C-52 |
| F2 | "No retry on 4xx" drops 401/403/408/429; rejected entries vanish from the pending count. | Classify by the RFC 9457 `retryable` member. Non-final outcomes stay pending. Final rejections are dead letters, kept, counted in status and in freshness. No ack without a final outcome. | R-FAIL-2 / C-53 |
| F3 | An event the connector cannot read is dropped and the cursor moves past it forever. | Park as `blocked_unknown_schema`; hold the cursor; count drops by reason; retry after upgrade. | R-FAIL-3 / C-54 |
| F4 | A replica offline longer than tombstone retention resurrects deleted records. | The endpoint answers `cursor_expired` (§7.4); the connector re-lists the project and purges every local copy that is tombstoned or absent. | R-CHG-8 / C-43 |
| F5 | Revocation is enforced only when the connector talks to the endpoint; a transient 401 quarantines a project. | Per-project access lease enforced by the daemon (default 7 days, PROPOSED; set with OQ-6); 401 refreshes, 403 or `access.revoked` revokes; refresh is atomic. | R-AUTH-9, R-AUTH-10 / C-48, C-49 |
| F6 | `conflict_recorded` counts as success and nothing surfaces it. | Count per project in status; mark the conflict set in recall. | R-CONF-5 / C-45 |
| F7 | The BM25 lane drops on a full channel while ingest returns `applied` and the cursor advances. | Non-dropping enqueue or durable backfill entry before `applied`; cursor never ahead of durable state. | R-FAIL-4 / C-55 |
| F8 | A secret-gate block has no outbox fate; a later revision with the blocked one as parent leaves the remote with an unknown parent. | Hold the blocked entry and every descendant revision; count held entries. | R-SAFE-4 / C-51 |
| F9 | Freshness is whatever the connector last reported; a dead connector looks fresh. | The daemon derives staleness from report age and connector liveness (stale after 3 report intervals or when the child is not running). | R-FAIL-5 / C-56 |
| F10 | A local forget of an imported record emits a delete of another principal's memory, or is undone by the next resync. | Suppression marker only; no delete event for a record the user did not author; resync honours the marker. | R-CHG-7 / C-42 |
| F11 | A purge leaves copies in derived indexes or queues. | Purge scope: drawer store, vector index, BM25 index, closet index, KG triples the record originated, outbox, connector retry and dead-letter queues, quarantine; consolidations listing it in `derived_from` are recomputed without it or purged. | R-FAIL-6 / C-57 |
| F12 | A consolidation of mixed sources inherits eligibility from one of them. | Eligible only if every `derived_from` source is eligible for the same project. | R-ELIG-5 / C-36 |

## 7. Remote memory-store API proposal {#SPEC-MEMSYNC-05~draft}

All PROPOSED. HTTP and JSON, described in OpenAPI 3.x. Events use the CloudEvents 1.0.2 envelope with extension attributes `projectid`, `memoryid` and `revisionid` (CloudEvents requires lower-case attribute names without underscores). Errors use RFC 9457 problem details with extension members `retryable` and `conflicting_heads`. Physical storage, database choice and ranking stay outside the contract.

### 7.1 Operations

| Operation | Shape | Notes |
|---|---|---|
| Capabilities | `GET /.well-known/trusty-memory-sync` | API and schema versions, `endpoint_id`, batch limits, event kinds, tombstone retention window, revocation-latency bound. No query or semantic-search capability is defined in v1: the connector is sync-only. |
| Authorization metadata | `GET /.well-known/oauth-protected-resource` | RFC 9728 metadata naming the endpoint's authorization servers. The client uses it only to check against its configured list (§8, R-AUTH-11). |
| Project binding | `GET /v1/projects/{project_id}` | Repository binding (for example `github:repo:<numeric id>`), authority endpoint id, the caller's role, policy flags. Used by the bind check (§5.6). |
| Publish | `POST /v1/projects/{project_id}/events:batch` | Up to N events. Idempotency key is CloudEvents `source` + `id`; a replay returns the original per-event outcome. Optional `expected_heads` per memory lets a client ask to fail rather than create a sibling. Outcomes: `accepted`, `duplicate`, `conflict_recorded` (sibling head created), `rejected` (with `retryable`). |
| Change feed | `GET /v1/projects/{project_id}/changes?cursor=&limit=` | Ordered events including tombstones, retractions, `access.revoked` and `project.binding.changed`; returns `next_cursor`. A cursor older than the retention window gets `cursor_expired` (§7.4). Long-poll or SSE MAY be added as a capability. |
| Batch get | `POST /v1/projects/{project_id}/records:batchGet` | By memory id, optionally by revision id. |
| List | `GET /v1/projects/{project_id}/records?page=` | Current heads and tombstones; used for full re-list. |
| Revision history | `GET /v1/projects/{project_id}/records/{memory_id}/revisions` | Includes conflict heads. |

### 7.2 Endpoint obligations

- Bind the authenticated principal to `author` or reject (C-19). Enforce project authorization per operation on every call (R-AUTH-1, R-AUTH-2).
- Reject envelopes that fail the closed schema, §5.4/§5.5 rules or the endpoint-conformant content checks of §5.7 (C-03, C-04, C-31, C-32, C-34).
- Recompute `revision_id` on publish and reject a mismatch. Record `attested_by` as its own `endpoint_id` when it bound the author.
- Never forward an inbound token to another service (no token passthrough).
- Translate provider signals (for example GitHub webhooks) into its own `access.revoked` and `project.binding.changed` events, and run periodic reconciliation, because provider webhooks are not redelivered automatically (research, standards-auth §7).
- Publish its tombstone retention window and its revocation-latency bound in capabilities (R-AUTH-8).

Inbound trust on the client (R-ID-7): each project has one authority endpoint, named in the project binding and fixed at bind time. Author attestations from any other endpoint are stored as unattested and shown so in recall. Signatures may be added later; they are not part of v1.

### 7.3 Multi-endpoint behavior

Client-mediated first: a connector subscribes to several endpoints per project and merges by ids. Store-to-store federation is an optional later capability and is not implied. An imported record is never republished by default (R-CHG-3, C-12).

### 7.4 Cursor expiry

An endpoint keeps tombstones for its published retention window. A change-feed request whose cursor predates the oldest retained event gets HTTP 410 with problem type `cursor_expired`. The connector then pages `GET …/records`, reconciles local imported state against the listed heads and tombstones, purges anything tombstoned or absent (§6.4 F11 scope), and resumes the feed from the cursor returned with the list (R-CHG-8, C-43).

## 8. Authentication and authorization options {#SPEC-MEMSYNC-06~draft}

GitHub is evaluated, not assumed (R-AUTH-3). Source facts and URLs are in `docs/specs/research/research-standards-auth.md` (read 2026-10-04). The choice of identity provider is OQ-5 (open).

| Option | Summary | Assessment |
|---|---|---|
| A. GitHub App, endpoint-issued tokens | Human signs in with a GitHub App user token (device flow for the headless connector). The endpoint verifies it once, records `github:user:<id>`, and issues its own short-lived, audience-restricted, project-scoped token. The GitHub token never leaves that exchange. | Recommended first identity provider. GitHub Apps give per-repository installation scope and short-lived tokens; a user token only carries permissions both the user and the app have. |
| B. GitHub OAuth App | Classic OAuth scopes. | Not recommended: the `repo` scope covers every repository the user can reach. |
| C. Endpoint accepts GitHub tokens directly | No exchange. | Rejected: token passthrough, no audience restriction, couples every backend to GitHub. |
| D. Provider-neutral endpoint authorization server | The endpoint (or its authorization server) federates any IdP; GitHub is one IdP behind it. | Required at the API level so the contract does not hard-code GitHub (C-69). A and D combine: the API is D, the first deployment uses A. |
| E. Workload identity | Cloud agents and CI use GitHub Actions OIDC. The endpoint binds a workload to `job_workflow_ref` or a protected `environment`, not only to `repository_id`/`repository_owner_id`, so an arbitrary workflow in the repository cannot mint. Recorded as a workload acting on behalf of `actor_id`; publishes only `agent_observation`, never `decided` (R-AUTH-12). | Later stage, for cloud sessions. |

Authorization position (PROPOSED, not a ruling): repository access is necessary but not sufficient. A repository administrator creates an explicit binding `project_id` to `github:repo:<id>`; the memory service keeps its own per-project membership and role. Public repository visibility grants nothing (R-AUTH-4). Whether private-repository collaborators get membership automatically is OQ-5b (open).

Client rules (PROPOSED):

- Authorization servers come from a list configured out of band (connector state directory, per endpoint). RFC 9728 metadata from the endpoint is checked against that list and never extends it; a mismatch stops sign-in before a device-flow code is shown (R-AUTH-11, C-50). HTTPS is mandatory for endpoints and authorization servers; plain-HTTP loopback is accepted only when the sandbox harness sets it.
- Clients defend against authorization-server mix-up (RFC 9207 `iss`, or a distinct redirect URI per server) and request audience-bound tokens (RFC 8707).
- Credentials live in the OS keychain through `crates/trusty-common/src/credentials`; only item names are configured. A refresh writes the new credential before discarding the old one, as one keychain update; a crash mid-refresh leaves one usable credential or a clear re-authentication state (R-AUTH-10, C-49). With option A, GitHub's rotating refresh token is held by the endpoint's token exchange, not by the connector; the connector holds only the endpoint-issued refresh token.

## 9. Vendor interoperability (engineering projects only) {#SPEC-MEMSYNC-07~draft}

Adapters are optional, project-scoped, and never a recall path. Detail, schemas and citations are in `docs/specs/research/research-vendor-interop.md` (read 2026-10-04). Chat-history APIs are not memory sync.

| Surface | What exists | Verdict | Mapping constraint |
|---|---|---|---|
| Anthropic client-side memory tool | Application-implemented file operations under `/memories` | Adapt | A project-scoped file view over local records; create, edit and delete map to revisions and tombstones. |
| Anthropic Managed Agents memory stores | Beta API with versions | Adapt, later | Workspace scope is not project scope; bind explicitly. Old versions may be deleted after 30 days. |
| OpenAI Sandbox Agents memory | Files carried across runs | Adapt cautiously | Model-consolidated Markdown; treat as `assistant_synthesis` import only. |
| Codex local memory | Generated local files | Export only | Never mutate generated files as a contract. |
| Claude app memory, ChatGPT memory | No sync API; text import or export only (UNVERIFIED: the ChatGPT help pages returned HTTP 403 and the Claude app memory blog was seen only as a search snippet; `docs/specs/research/research-vendor-interop.md` §4 and line 29) | Avoid | Whole-profile import is out of scope. |
| Agents SDK sessions, Responses and Conversations APIs | Chat history | Avoid | Not semantic memory. |

## 10. Dream worker {#SPEC-MEMSYNC-11~draft}

All PROPOSED. Requirements: DOC-79 §13 (R-DREAM-1 to R-DREAM-11). Decision: [ADR-0068](../adr/0068-memory-sync-and-dream-run-as-daemon-supervised-child-processes.md) (Bob, 2026-10-04 scope addition). Today's code path: §2.8. Runtime work is sequenced after the fixes for [#9172](https://github.com/bobmatnyc/trusty-tools/issues/9172) and [#9173](https://github.com/bobmatnyc/trusty-tools/issues/9173), which edit the same `dream/` modules; the Architect assigns it.

### 10.1 Split: the worker decides, the daemon applies

The worker cannot open palace storage (§2.2), so every pass splits into a decision step in the worker and an apply step in the daemon.

| Today's pass (§2.8) | Worker | Daemon |
|---|---|---|
| Content prune | Selects drawers by blocklist and word count from the snapshot. | Applies `prune` actions. |
| Dedup | Asks for neighbour scores, picks the survivor by the rule the [#9172](https://github.com/bobmatnyc/trusty-tools/issues/9172) fix sets (current drawer preferred), builds the merged content. | Runs the neighbour search; applies `merge` actions. |
| Importance prune | Selects by effective importance and age. | Applies `prune` actions. |
| Semantic consolidation (optional LLM) | Calls the configured inference backend; builds canonical drawers. | Applies `consolidate` actions: writes canonical drawers and `superseded_by` triples. |
| Fading detection | Computes from the snapshot; reports it. | Stores it with the stats. |
| Vector compaction, closet refresh, L1 flush, KG compaction, recall benchmark | Requests them. | Runs them on `memory.dream_maintain`; they take no decision input. Placement is OQ-12 (open). |
| `dream_stats.json` | Reports stats. | Writes the file and dream status. |

### 10.2 Daemon methods (dream family)

Every method requires the dream worker's spawn secret (§3.1); the connector and any other caller are refused (R-SYNC-8, R-PROC-5).

| Method | Behavior |
|---|---|
| `memory.dream_next` | Returns the next work item `{palace_id, trigger: scheduled or manual, request_id, room, options}` or `idle` or `shutdown`. The daemon owns one queue: palaces idle by the per-handle access clock (§2.8), plus manual requests. One open item per palace. |
| `memory.dream_snapshot` | Paged drawer rows for one palace: id, content, content hash, importance, `created_at`, access stats, type, tags, `fact_key`, origin (local or imported), read-only eligibility and decision fields when present, and a snapshot generation. Never touches the access clock. |
| `memory.dream_neighbours` | Batch vector-lane neighbour ids and scores for a list of drawer ids. Never touches the access clock. |
| `memory.dream_apply` | One action (`merge`, `prune`, `consolidate`) with preconditions (each touched drawer's presence and content hash). Applied atomically under the per-palace write lock with its maintenance-journal record; returns `applied`, `stale` or `refused` with a reason. |
| `memory.dream_maintain` | Runs named storage passes for one palace and returns per-pass results. |
| `memory.dream_report` | Stats, fading list and completion for a work item. The daemon persists `dream_stats.json`, updates dream status and completes any manual request. |

`memory.dream_status` and `memory.palace_dream_status` (§2.7) gain additive keys: worker state (`running`, `restarting`, `given_up`, `disabled`), last exit reason, last report age, last interrupted pass.

### 10.3 Refusal rules in `memory.dream_apply`

The daemon refuses an action, changes nothing and logs the reason when:

- it touches an imported drawer (R-DREAM-7, C-64);
- it sets or widens eligibility, changes a project binding, or moves a record toward `decided` (R-DREAM-6, C-63);
- it would merge or prune a protected `Task` drawer (the existing rule, VERIFIED `crates/trusty-common/src/memory_core/dream/cycle.rs:369-373`);
- a precondition no longer holds (R-DREAM-9, C-65);
- the daemon no longer holds the maintenance lease or is shutting down.

### 10.4 Data integrity on a worker crash

- **Unit of change.** Each `dream_apply` is one atomic unit in the daemon. A `merge` commits the survivor's new content to durable storage (not only the in-memory table), removes the loser from every index, and writes the journal record naming survivor and score, together or not at all (R-DREAM-3, R-DREAM-4). A `consolidate` commits its canonical drawers, their `superseded_by` triples and any removals the same way.
- **Crash points.** A worker crash before the call changes nothing. A crash during the call does not interrupt it: the daemon finishes or rolls back the transaction it received, independent of the caller. A crash after the call leaves a complete action. No crash point leaves a partial merge or an unrecorded removal (C-61).
- **Partial pass.** A pass is a sequence of independent actions. A crash between actions leaves some applied and some not, each complete. When the worker dies with an open work item, the daemon marks the pass `interrupted` in dream status and requeues the palace.
- **Known defect.** Today a dedup merge rewrites only the in-memory table, so merged text is lost at restart, and two survivors were removed without a journal record ([#9172](https://github.com/bobmatnyc/trusty-tools/issues/9172); related [#8729](https://github.com/bobmatnyc/trusty-tools/issues/8729)). The apply primitive is built on that fix, not beside it.

### 10.5 Lifecycle

- **Spawn and restart.** §3.1 applies unchanged. `TRUSTY_DREAM_DISABLED` set: no worker is spawned and status says `disabled` (R-DREAM-11, C-68).
- **Give-up.** Scheduled work waits in the queue; nothing else changes. Recall, remember, forget and sync never depend on the worker (R-DREAM-5, C-62).
- **Manual triggers.** `memory.dream_run` and `dream_consolidate_room` enqueue a manual item and wait for its report. When the worker is not running, they return an "unavailable" error at once. The in-process dream path is removed, so there is no fallback (R-DREAM-10, C-67).
- **Shutdown.** `dream_next` returns `shutdown`; the daemon sends SIGTERM and waits the grace period; an apply already received completes; later applies are refused. Unapplied decisions are discarded and the pass is recorded as interrupted (R-DREAM-8, C-66).
- **Concurrency.** `TRUSTY_DREAM_MAX_CONCURRENT` bounds passes inside the worker. The daemon serializes `dream_apply` per palace under the existing write lock.
- **Access clock.** The daemon sets `is_compacting` only for the duration of each apply or maintain call, so dream work never counts as user access (§2.8).

### 10.6 Relation to [#9173](https://github.com/bobmatnyc/trusty-tools/issues/9173)

The per-palace loop model (64 loops for about 104 palaces in the issue) disappears: one worker pulls from one daemon queue. The #9173 fix still lands first in today's scheduler; the queue reuses its palace-selection rule.

## 11. Ownership boundary with the runtime owner {#SPEC-MEMSYNC-08~draft}

The runtime owner, assigned by the Architect, owns all runtime trusty-memory work in flight, installs, daemon restarts, releases, version bumps and the shared `trusty-common` crate (DOC-79 §15). This design changes none of it. Each row is a dependency to route through the Architect. Milestone-to-issue mapping in this table is inferred from branch and issue references; Architect to confirm.

| Runtime item | What sync or the dream worker needs from it | When |
|---|---|---|
| M1 (memory spike: reclaim, palace store, HNSW replay, recall-all; inferred [#9140](https://github.com/bobmatnyc/trusty-tools/issues/9140), [#9141](https://github.com/bobmatnyc/trusty-tools/issues/9141)) | Nothing now. Later: the reclaim must not delete a palace with unacknowledged outbox entries, held or dead-letter entries, or an active sync binding. | Before Stage 2 |
| f0 reclaim (empty-palace reclaim to dated trash) | Same reclaim guard as M1. | Before Stage 2 |
| M2 (recall ranking, superseded demotion, rulings; inferred [#9142](https://github.com/bobmatnyc/trusty-tools/issues/9142), [#9143](https://github.com/bobmatnyc/trusty-tools/issues/9143), [#8246](https://github.com/bobmatnyc/trusty-tools/issues/8246)) | Ranking that reads `decision_state` so a `tentative` record never outranks a `decided` one (R-CONT-6), and demotes `superseded`. | After M2 merges |
| M3 (short ids) | Sync uses its own `memory_id`; it must not depend on the short-id format. No change requested. | — |
| Recall path (request, not committed) | Recall output marks imported records, conflict-set members and untrusted imported text with provenance, lists canonical refs, and excludes quarantined and suppressed records (R-SYNC-6, R-SAFE-3, R-CONF-5, R-CHG-7, C-06, C-14, C-22, C-45, C-48). | Stage 2 to 3; Architect sequencing required |
| [#9172](https://github.com/bobmatnyc/trusty-tools/issues/9172) dream dedup | Durable merges, current-drawer survivor choice and journaled survivor removal; the dream apply primitive (§10.4) builds on it. Dream passes also skip imported records (R-DREAM-7). | Before Stage D |
| [#9173](https://github.com/bobmatnyc/trusty-tools/issues/9173) dream scheduler | Its palace-selection fix is reused by the daemon's dream queue (§10.6). | Before Stage D |
| [#9174](https://github.com/bobmatnyc/trusty-tools/issues/9174) vector lane above 4,096 drawers | C-01 and C-17 assume imported records are found by the vector lane; `memory.dream_neighbours` relies on the same lane. | Before Stage 3 |
| `trusty-common` `memory_core` | Typed drawer fields (decision state, claim kind, expression, refs, memory id, provenance; OQ-4 decided), the outbox append in the write and forget paths in one transaction, suppression markers, tombstones, a secret gate on the share path, the dream decide/apply split. All additive and need `#[non_exhaustive]` care (ADR-0051 decision 4). | Stage 1 and Stage D; Architect sequencing required; not committed |
| Releases and installs | Neither child cuts a release or installs a binary; code ships through local-ops on the normal cadence. | Always |
| CI and build capacity | Stage 0 needs no builds. Later stages use crate-scoped gates and request build slots through the Architect under the machine's builder cap. | Always |

## 12. Conformance suite mechanics {#SPEC-MEMSYNC-09~draft}

PROPOSED. The suite runs DOC-79 §14's cases C-01 to C-69.

- **Fake endpoint.** An in-process endpoint in the wire-types crate implements §7 with an in-memory store, fault injection (duplicate delivery, reordering, stalls, 401/403/408/410/422/429/503, `access.revoked`, unknown schema versions, tampered content), and a recorder for assertions. It can run several instances (E1, E2) in one test, so two-endpoint cases need no real endpoint. It is the default target in CI.
- **Two machines.** Two data roots and two daemons on one host, each started through `scripts/sandbox_daemon.sh` so no test touches a user keychain or bot token. Each daemon spawns its own children.
- **Child fault injection.** A test-only build feature adds named crash points in both children and in the daemon's apply paths, plus a hook to fail the outbox append (C-52, C-61). An open-file audit of each child supports C-28.
- **Endpoint conformance.** The same cases run against any external endpoint URL given at run time; a reference endpoint claims conformance only by passing them.
- **Addendum E evals.** C-03 to C-09, C-31 to C-35 run as schema and policy tests plus one evaluator-agent case (C-09) with a fixed scoring rubric that gives no credit for reproduced ticket text.
- **Baselines.** Stage 1 measures recall p50/p95 with sync disabled and with the in-process dream cycle; C-17 and C-62 compare against it.

## 13. Decisions and open questions

### 13.1 Decided (Bob, 2026-10-04)

1. **OQ-1 Process placement — decided: (a) a child process supervised by the daemon.** Bob's ruling said "operationally analogous to the dream cycle" and "do not silently reduce it to an in-process async task"; the analogy holds for lifecycle. The same day Bob widened the scope: the dream cycle also moves into a daemon-supervised child, under the same supervision model (§3.1, §10, [ADR-0068](../adr/0068-memory-sync-and-dream-run-as-daemon-supervised-child-processes.md)).
2. **OQ-2 Shared project identity — decided: (a) a project UUID committed in the pin file, plus per-provider bindings.** `github:repo:<id>` is an authorization binding, not the identity. Safeguard added from review r1: the pin file supplies only a candidate; sync requires an explicit local bind, and the endpoint's repository binding must match the checkout's remote (§5.6, R-PROJ-6, C-27).
3. **OQ-3 ADR-0051 fields — decided: (b) keep sync identity out of `palace.json`**, in the sync binding and the envelope only. ADR-0051 is amended afterwards to add the project UUID, so `palace.json` gains one coherent identity change (R-PROJ-7).
4. **OQ-4 Decision state and canonical links locally — decided: (a) typed drawer fields in `trusty-common`**, added after M2. Routed through the Architect (§11).

### 13.2 Open

Each question gives options and a recommendation. Bob explicitly left OQ-5, OQ-6 and OQ-7 undecided on 2026-10-04.

5. **OQ-5 Identity provider.** (a) GitHub App with endpoint-issued tokens; (b) OAuth App; (c) provider-neutral API with GitHub App as the first IdP. **Recommendation: (c).** Sub-question OQ-5b: do private-repository collaborators get memory membership automatically? Options: automatic, admin-approved, or invite-only. **Recommendation: admin-approved**, so repository access stays necessary but not sufficient.
6. **OQ-6 Local cache policy on revocation and lease lapse.** Options: retain, quarantine, purge. **Recommendation: quarantine by default, purge on operator action or after 30 days; access lease (R-AUTH-9) of 7 days.**
7. **OQ-7 Who may correct, retract or delete another principal's record.** Options: author only; author or project administrator; any project writer by new revision only. **Recommendation:** any writer may supersede with a new linked revision; only the author or a project administrator may retract or delete. C-44 runs against whichever policy is configured.
8. **OQ-8 A `decided` record with no canonical artifact yet.** Options: forbid; allow with `record_gap: no_artifact_yet`; allow silently. **Recommendation: allow with the flag**, so emerging decisions are kept and readers see that no canonical home exists yet.
9. **OQ-9 Tier C (`fact_key`) records.** Options: never sync; sync with slot semantics. **Recommendation: never in v1.**
10. **OQ-10 Default outbound eligibility.** Options: per-record opt-in; per-project policy by claim kind; everything not private. **Recommendation: per-project policy by claim kind, off by default**, with explicit per-record publish always available (R-SYNC-7). Unclassified drawers stay ineligible under every option (R-CONT-11).
11. **OQ-11 Requirements home.** Options: DOC-79 in `docs/specs/` (as written); a PRD under `docs/prd/`. **Recommendation: keep DOC-79**; there is no requirements directory and DOC-79 carries anchored IDs the design links to.
12. **OQ-12 Where the storage-only dream passes run** (vector compaction, closet refresh, L1 flush, KG compaction, recall benchmark). Options: (a) in the daemon on the worker's `memory.dream_maintain` request; (b) on a daemon timer with no worker involvement; (c) in the worker through finer-grained RPCs. **Recommendation: (a).** These passes take no decision input, need the store's locks, and keeping them worker-triggered keeps one schedule; (c) would move bulk index data over the socket for no isolation gain.

## 14. Staged plan {#SPEC-MEMSYNC-10~draft}

PROPOSED. Each stage starts only on Bob's GO through the Architect. Rungs refer to the repository test ladder. Every case listed for a stage keeps passing in later stages.

| Stage | Content | Exit criterion | Depends on |
|---|---|---|---|
| 0 Docs | DOC-79, DOC-80, ADR-0068 to ADR-0070; Bob answers §13.2. | ADRs accepted or amended; OQ-5 to OQ-12 answered. | — |
| 1 Local prerequisites | Typed drawer fields (OQ-4), project UUID and bind (OQ-2), outbox in the drawer transaction including user forgets, suppression markers, tombstones, secret gate on the share path, recall baseline measurement. | Rung 4 gates on `trusty-common` and its direct dependents; baseline numbers recorded. | Architect sequencing (§11); M2 merged |
| 2 Connector skeleton, one machine | Child supervisor, connector subcommand, spawn secret, sync RPCs, status keys, wire-types crate, multi-instance fake endpoint. | C-02 to C-04, C-11, C-12 (two fake instances), C-17, C-18, C-23, C-25, C-26, C-28 (connector), C-30 (sync methods), C-31 to C-34, C-51 to C-56, C-58 (connector), C-59. Rung 5 (process lifecycle). | Stage 1; M1 and f0 reclaim guard |
| D Dream worker | Dream decide/apply split, dream RPCs, worker subcommand on the Stage 2 supervisor, removal of the in-process dream path. | C-28 (worker), C-30 (dream methods), C-58 (worker), C-60, C-61, C-62, C-65 to C-68; C-63 and C-64 once Stage 2's import marking exists. Rung 5. | Stage 2 supervisor; fixes for [#9172](https://github.com/bobmatnyc/trusty-tools/issues/9172) and [#9173](https://github.com/bobmatnyc/trusty-tools/issues/9173) merged |
| 3 Two users, two machines | Full sync suite on the fake endpoint and two sandboxed daemons; Addendum E evals. | C-01, C-05 to C-10, C-13, C-15 (two fake instances), C-16, C-22, C-24, C-27, C-29, C-35 to C-38, C-41 to C-45, C-57; recall stays within baseline. | Stage 2; [#9174](https://github.com/bobmatnyc/trusty-tools/issues/9174); recall-path request (§11) |
| 4 Auth and reference endpoint | Endpoint token exchange, GitHub App option, access lease, revocation events and reconciliation; reference endpoint repository if ADR-0070 is accepted. | C-14, C-19 to C-21, C-39, C-46 to C-50, C-69 against the reference endpoint. | Bob's GO on repository creation and app registration; OQ-5, OQ-6 |
| 5 Mesh | Several real endpoints per project, endpoint migration, resharing policy. | C-12 and C-15 pass again with two real endpoints. | Stage 4 |
| 6 Adapters and workloads | Vendor adapters (§9), cloud workload identity. | C-40; per-adapter conformance subset. | Stage 5 |

## 15. Proposed epic and issue breakdown (not filed)

Drafted for the Architect to relay. Nothing below is filed; numbers are placeholders. "Architect assigns" means the Architect picks the owner and sequencing; nothing here commits the runtime owner.

**Epic: Shared engineering-project memory sync and dream worker (DOC-79, DOC-80).**

| # | Proposed issue | Crate | Rung | Stage | Owner path |
|---|---|---|---|---|---|
| E1 | Decide §13.2 open questions; move ADR-0068 to ADR-0070 out of Proposed; amend ADR-0051 per OQ-3 | docs | 1 | 0 | Bob via Architect |
| E2 | Portable project UUID in the pin file, bindings, explicit bind with repository check | `trusty-common`, `trusty-memory` | 4 | 1 | Architect assigns; sequencing required, not committed |
| E3 | Typed drawer fields: decision state, canonical refs, claim kind, expression, memory id, provenance roles | `trusty-common` | 4 | 1 | Architect assigns; sequencing required, not committed |
| E4 | Per-palace sync outbox in the drawer transaction, user forgets as delete events, suppression markers, reconciliation scan | `trusty-common`, `trusty-memory` | 5 | 1 | Architect assigns; sequencing required, not committed |
| E5 | Secret gate on the share export and import paths | `trusty-common` | 3 | 1 | Architect assigns (closes the gap in `docs/reference/shared-memory-identity.md`) |
| E6 | Recall ranks by decision state; recall markers for imported, conflict, untrusted; quarantine and suppression exclusion | `trusty-memory` | 3 | 1–3 | Architect assigns |
| E7 | Recall latency baseline harness | `trusty-memory` | 2 | 1 | Architect assigns |
| E8 | Wire-types crate: envelope, nested closed schema, events, OpenAPI and JSON Schema | new crate | 3 | 2 | Architect assigns |
| E9 | `memory.sync_ingest`, outbox read/ack, `memory.sync_report`, status keys, daemon-derived staleness | `trusty-memory` | 5 | 2 | Architect assigns |
| E10 | Child supervisor, spawn secret, connector subcommand | `trusty-memory` | 5 | 2 | Architect assigns |
| E11 | Multi-instance fake endpoint and conformance harness, including fault injection | new crate | 2 | 2 | Architect assigns |
| E12 | Dream passes skip imported records | `trusty-common` | 3 | D | Architect assigns (with [#9172](https://github.com/bobmatnyc/trusty-tools/issues/9172)) |
| E13 | Reclaim guard for palaces with sync state | `trusty-memory` | 3 | 2 | Architect assigns (with M1 / f0) |
| E14 | Two-machine conformance run and Addendum E evals | `trusty-memory` | 5 | 3 | Architect assigns |
| E15 | Endpoint token exchange, GitHub App option, access lease, authorization-server allow-list | wire-types crate, reference endpoint | 5 | 4 | Architect assigns, on Bob's GO |
| E16 | Reference endpoint repository | separate repository | — | 4 | Bob's GO required |
| E17 | Console freshness and child-health view | `trusty-console` | 6 | 4 | Architect assigns |
| E18 | Multi-endpoint subscriptions and migration | `trusty-memory` | 5 | 5 | Architect assigns |
| E19 | Vendor adapters, one issue each | adapter crates | 3 | 6 | Architect assigns, later |
| E20 | Dream apply primitive: atomic `merge`/`prune`/`consolidate` with journal record and preconditions | `trusty-common` | 5 | D | Architect assigns, after the [#9172](https://github.com/bobmatnyc/trusty-tools/issues/9172) fix |
| E21 | Dream daemon methods (`dream_next`, `dream_snapshot`, `dream_neighbours`, `dream_apply`, `dream_maintain`, `dream_report`) and status keys | `trusty-memory` | 5 | D | Architect assigns, after the [#9173](https://github.com/bobmatnyc/trusty-tools/issues/9173) fix |
| E22 | Dream worker subcommand: decide steps moved out of the daemon, inference calls in the worker | `trusty-memory`, `trusty-common` | 5 | D | Architect assigns |
| E23 | Manual dream triggers routed to the worker; in-process dream path removed | `trusty-memory` | 5 | D | Architect assigns |
| E24 | Dream conformance cases C-58 (worker), C-61 to C-68 | `trusty-memory` | 5 | D | Architect assigns |

## 16. Sources

- Owner direction: the 2026-10-04 brainstorming note and session brief (Addenda A to G), held outside the repository by the Architect; owner decisions on OQ-1 to OQ-4 and the dream-worker scope addition, 2026-10-04.
- Review: code-analyzer review r1 of these documents, 2026-10-04 (held by the Architect). Finding dispositions are recorded in the r2 change log, not here.
- Research, dated 2026-10-04, in `docs/specs/research/`: `docs/specs/research/research-code-internals.md`, `docs/specs/research/research-standards-auth.md`, `docs/specs/research/research-vendor-interop.md`, `docs/specs/research/research-conventions-ownership.md`.
- Repository references: `docs/reference/shared-memory-identity.md`; ADR-0022, ADR-0027, ADR-0028, ADR-0032, ADR-0043, ADR-0051, ADR-0054, ADR-0062, ADR-0065 in `docs/adr/`; closed vision issue [#1683](https://github.com/bobmatnyc/trusty-tools/issues/1683); open defects [#8729](https://github.com/bobmatnyc/trusty-tools/issues/8729), [#9172](https://github.com/bobmatnyc/trusty-tools/issues/9172), [#9173](https://github.com/bobmatnyc/trusty-tools/issues/9173), [#9174](https://github.com/bobmatnyc/trusty-tools/issues/9174).
- External standards named here (CloudEvents 1.0.2, W3C PROV-O, RFC 9110, RFC 9457, RFC 9700, RFC 9207, RFC 8707, RFC 9728, RFC 8693, OpenAPI 3.x) are cited with URLs and read dates in the standards research file.
