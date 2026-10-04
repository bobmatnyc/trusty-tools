# trusty-memory internals as input to a shared project-memory sync design

Repo: /Users/masa/trusty-mpm-projects/bobmatnyc/trusty-tools, main checkout, HEAD e3ff4c7358, tree clean at start. Read-only. No cargo run.
Paths are repo-relative. `tm` below means the trusty-memory crate (`crates/trusty-memory`), `tc` means `crates/trusty-common`.
VERIFIED = I read the cited lines. INFERENCE = my reading, no direct citation; labelled each time.

## 0. Findings that change the brief's premises

1. VERIFIED: the dream cycle is NOT a separate process. It is per-palace `tokio::spawn` tasks inside the daemon. "Separate process supervised like the dream cycle" has no precedent in the dream path (see Q1). The nearest precedents are `EmbedderSupervisor` and `UdsServiceSupervisor` in trusty-common.
2. VERIFIED: a cross-machine memory exchange primitive already exists and is not wired to anything. `tc` `memory_core::share` (#5902) defines a JSONL record, a content-hash identity, and an idempotent import. No CLI command, RPC method or MCP tool calls it (Q3, Q6).
3. VERIFIED: no external write interface can supply a drawer id or `created_at`. Every write path mints a fresh UUIDv4 and `Utc::now()` (Q2, Q3). Only the in-process `share::import` path preserves `created_at`, and it still mints a new id.
4. VERIFIED: the trusty-memory daemon has no HTTP surface. It serves one Unix socket (ADR-0032). A remote-store API would be a new listener or a trusty-console route (Q7).
5. VERIFIED: ADR-0051's `owner`/`project` fields on `Palace` are not in the code at HEAD (Q4). No palace GUID exists either.

## 1. Process model

### 1a. How the daemon starts
- VERIFIED `crates/trusty-memory/src/main.rs:989-1135` (`run_serve`). Without `--foreground`/`--http` it self-spawns a detached copy and returns (`main.rs:1001` calls `commands::start::handle_start`). With `--foreground` it runs inline.
- VERIFIED `main.rs:1009` `parent_death::arm_from_env` is opt-in, for tests only.
- VERIFIED `main.rs:1035` single-instance guard: probes the socket 3 times at 200 ms, and exits 0 if a healthy daemon is already serving.
- VERIFIED `main.rs:1064-1075` refuses to bind the production socket when a launchd unit owns it and launchd does not report it runs this PID.
- VERIFIED `main.rs:1124-1135`: builds `AppState::new(data_root).with_writer_intent()...`, calls `spawn_startup_tasks(&state)`, then `serve(state, &socket)`.
- VERIFIED `crates/trusty-memory/src/transport/uds.rs:403-405`: socket path comes from `trusty_common::daemon_socket_path("trusty-memory")`. Bind is `bind_singleton_hardened` (`uds.rs:407-428`).

### 1b. How the dream cycle runs
- VERIFIED in-process tasks. `crates/trusty-memory/src/startup_tasks.rs:135` calls `dream_scheduler::spawn_background_maintenance`.
  - `crates/trusty-memory/src/dream_scheduler.rs:81-143` `spawn_dream_scheduler` loops over the registry's palaces and calls `dreamer.start_with_shutdown(...)` (`:122`).
  - `crates/trusty-common/src/memory_core/dream/dreamer.rs:138-180`: `tokio::spawn` at `:145`. The loop sleeps `idle_secs + stagger`, selects on a shutdown `watch`, then runs `dream_cycle` (`:177`).
  - `dream_scheduler.rs:160-171` also spawns the idle-evict ticker on the same shutdown watch.
  - `dream_scheduler.rs:208-214`: the shutdown bridge flips the `watch` on SIGTERM/SIGINT.
- VERIFIED `dream_scheduler.rs:81-90`: `TRUSTY_DREAM_DISABLED` turns scheduling off. `dream_max_concurrent()` bounds concurrent cycles process-wide (`tc` `memory_core/dream/concurrency.rs:107`).
- VERIFIED maintainer election across processes: `tc` `memory_core/maintenance_lease.rs:1-20` (flock on `<data_root>/maintenance.lock`). The dream loop checks `registry.may_run_maintenance()` each tick (`dreamer.rs:167`, `tc` `memory_core/registry.rs:320-324`). Only the lease holder runs maintenance. User writes are not elected.

### 1c. Who supervises the daemon
- VERIFIED launchd, macOS only. `crates/trusty-memory/src/commands/service.rs:157-170` builds a plist with `KeepAlive::OnSuccess`, `throttle_interval: 10`, `serve --foreground`, and an fd limit of 8192. Clean exit 0 does not respawn, a non-zero exit does.
- VERIFIED the daemon has no in-process health watchdog for background tasks. A panic inside one dream task does not restart that task (`dreamer.rs:145-179` has no restart wrapper). `worker_liveness.rs` tracks in-flight work for wedge detection (`crates/trusty-memory/src/worker_liveness.rs:53-205`), not restart.
- Status surface for the dream loop: `memory.dream_status` and `memory.palace_dream_status` (`crates/trusty-memory/src/transport/methods/kg.rs:313-330`), registered at `transport/uds.rs:106-107`.

### 1d. Existing child-process supervision helpers (VERIFIED)
- `tc` `embedder_client/supervisor.rs`: `EmbedderSupervisor` (`:98`) supervises the `trusty-embedderd` sidecar. `start_supervisor_task` (`:228`) returns a `SupervisorHandle` with cooperative `shutdown()`. It restarts on non-zero exit and on a wedge signal, with exponential backoff `1<<n` seconds capped by `backoff_max_secs` (`:894`). Defaults: `max_restarts: 5`, cap 60 s (`embedder_client/supervisor_config.rs:84-85`). `has_given_up()` is at `:355`.
- `tc` `uds/supervisor/mod.rs:76` `UdsServiceSupervisor`, `ensure_running` at `:217`. It is on-demand spawn plus probe, with a per-instance key, an LRU cap, an RSS ceiling, `kill_on_drop`, and SIGTERM then SIGKILL. Liveness is decided by the socket, not `try_wait()` (`:18-26`). It is used by `trusty-console/src/webhook/spawn.rs` and `trusty-analyze/src/service/rpc.rs`. It is on-demand, not a continuous restart loop.
- `tc` `parent_death.rs:1-30`: a child exits when its spawner dies (pid plus start time, polled `getppid`).
- `tc` `spawn_retry.rs`: ETXTBSY-only retry for spawns.
- `tc` `supervision.rs:1-30`: `launchd_supervision()` asks launchd, not the environment, whether it runs this PID.
- Counter-precedent: `crates/trusty-memory/src/bm25_lane.rs:1-30` records that BM25 used to be a per-palace subprocess (`trusty-bm25-daemon`, `Bm25Supervisor`) and was collapsed in-process (#5329). The reason given: it was never enabled in any shipped config, and the spawn/probe/reap machinery carried risk for nothing. `ADR-0034` still describes `Bm25Supervisor`, but the type no longer exists in `crates/trusty-memory/src`.
- INFERENCE: a connector child that needs continuous restart fits `EmbedderSupervisor`'s shape (long-lived, backoff, give-up flag). A connector spawned on a schedule fits `UdsServiceSupervisor`. Neither is wired to trusty-memory today.

## 2. Write and ingestion boundary

### 2a. Every write interface into a palace
Transport (VERIFIED `crates/trusty-memory/src/transport/mod.rs:1-12`, `uds.rs:1-20`): one Unix socket, JSON-RPC, newline-framed. No HTTP, no discovery file.
- `memory_remember`: `crates/trusty-memory/src/tools/mod.rs:202` -> `tools/memory_ops.rs:36`. Schema `tools/definitions.rs:111-132`. Accepted params: palace, text, room, wing, tags, force, allow_secret_like, context, fact_key, expires_at, cwd, workstream. No id, no created_at, no importance, no drawer_type.
- `memory_note`: `tools/mod.rs:203` -> `memory_ops.rs:197`. Pinned `DrawerType::UserFact`, importance 1.0 (`definitions.rs:133`).
- `memory_forget`: `tools/mod.rs:222` -> `memory_ops.rs:389`.
- `task_add`, `task_complete`: `tools/mod.rs:254`, `tools/task_ops.rs:45`.
- `kg_assert`, `kg_retract_triple`: `tools/mod.rs:210-213`.
- `memory.drawer_create`, `memory.drawer_delete`, `memory.remember_async`: folded RPC methods (`transport/uds.rs:97-98,112`). `drawer_create` -> `service/core.rs:669-709` -> `remember_with_options` (accepts content, room, tags, importance, force; no id, no timestamps). `remember_async` queues a write (`transport/methods/admin.rs:97`).
- `chat_session_*`, `chat_turn_append`: separate redb chat store, not drawers (`tools/mod.rs:245-251`; `docs/specs/trusty-memory-chat-session-manager.md`).
- `kuzu` import (#277): `trusty-memory import kuzu`, `commands/kuzu_import/mod.rs:1-60`. VERIFIED it opens palaces in-process and refuses a real run while a daemon is running (`mod.rs:51-54`). Writes go through `remember_with_options` with `force: true` (`kuzu_import/apply.rs:457-463`) so the secret gate stays on. Idempotency uses tags `source:kuzu-memory/<id>` and `kuzu-hash:<hash>` (`mod.rs:30-38`).
- `share::import_palace_jsonl` / `import_palace_records` (#5902): library only in `tc` (`memory_core/share/import.rs:99,170`). Needs an in-process `PalaceHandle`; there is no RPC method for it.

Which one an external process would use:
- VERIFIED the supported external route is the UDS RPC. `tc` `memory_rpc.rs:234` `call_memory_tool`, `:250` `call_memory_tool_at`. `crates/trusty-memory/src/client.rs:1-25` is the in-crate equivalent. The MCP stdio bridge (`commands/serve_stdio_bridge.rs`) forwards to the same socket and injects `cwd` per request.
- VERIFIED consequence: via `memory_remember` a pulled memory gets a new UUID and `created_at = now`. The write also stamps `creator:*` tags with the CALLER's identity, not the original author's (`helpers.rs:720-745`, via `attach_mcp_attribution`). To preserve `created_at` the connector must either live in the daemon process and call `share::import`, or the RPC surface needs a new method.

### 2b. Serialization, locks, storage
- VERIFIED per-palace in-process write mutex: `PalaceHandle.write_mutex` (`tc` `memory_core/retrieval/handle.rs:168`), taken in `write_pipeline.rs:113-117` with a bounded wait (#906). A second, narrower `commit_mutex` (`handle.rs:193`, taken `write_pipeline.rs:364`) orders commits. The tool layer also holds `state.palace_write_lock(palace)` (`crates/trusty-memory/src/tools/memory_ops.rs:132`, `lib.rs:819`) across gate-check plus write (#230). The mutex is per palace; writes to other palaces run in parallel.
- VERIFIED KG writes go through one actor per palace: `tc` `memory_core/store/kg_writer.rs:1-40`. An mpsc queue coalesces up to 64 ops into one redb transaction, and callers await a oneshot after commit.
- VERIFIED storage:
  - redb `kg.redb` holds drawers (`DRAWERS` table, key = drawer UUID bytes, `tc` `store/kg_store.rs:70`), the fact-key slot index (`DRAWERS_BY_FACT_KEY`, `kg_store.rs:92`), triples, rooms and wings.
  - usearch/HNSW vector index lives in `index.usearch.redb` (`store/concurrent_open.rs:1-30` names the file).
  - An L1 JSON snapshot is rewritten on each write (`write_pipeline.rs:383`).
  - Optional BM25 lane, in-process (see 2c).
- VERIFIED cross-process behaviour: redb takes an exclusive flock per file. A second process opening with `OpenIntent::Writer` fails (`store/concurrent_open.rs:45-70`). A `ReadOnlyClient` opens a snapshot copy that rejects writes. So a separate sync PROCESS cannot open a palace for writing while the daemon holds it. It must go through the daemon.
- VERIFIED serialized write queue: there is no global queue. The serialization is the per-palace mutex plus the KG actor. A palace write waits at most the configured write-lock timeout and a pipeline budget (`write_pipeline.rs:84-185`).

### 2c. When indexes update
- Vector: VERIFIED synchronous by default, inside the write mutex (`write_pipeline.rs:307-340`: embed, then `vector_store.upsert`). When the daemon is `Warming`, `defer_embedding` is set and a background task embeds later (`write_pipeline.rs:376-378`, `handle.rs:807`). Failures retry with backoff and land in an embed ledger (`handle.rs:790-805`). Repair: `palace_reembed` (`tools/palace_ops.rs:282`).
- KG: VERIFIED redb commit is synchronous (`write_pipeline.rs:364-372`). Auto-extracted triples are best-effort after the write (`helpers.rs:641`).
- BM25: VERIFIED optional and OFF unless `TRUSTY_BM25_DAEMON=1` (`lib.rs:858-871`). When on, enqueue goes onto a bounded 256-slot channel that DROPS when full (`helpers.rs:621`, `bm25_backfill.rs:11-17`). `share::import::insert_new` never calls the BM25 enqueue (`share/import.rs:418-470`).
- Closet keyword index: rebuilt synchronously after each write (`write_pipeline.rs:388`; `share::import` does it once per batch, `import.rs:253-255`).
- `share::import::insert_new` also does not emit `DaemonEvent::DrawerAdded` or update `palace_last_used` (those live in the tool layer, `helpers.rs:626-633`, `tools/mod.rs:140-200`).

## 3. Record model

### 3a. Drawer (VERIFIED `tc` `memory_core/palace.rs:219-300`, persisted row `store/kg_store.rs:343-377`, `store/kg_redb/types.rs:210-228`)
| Field | Persisted | Notes |
|---|---|---|
| `id: Uuid` | row key | `Uuid::new_v4()` in `Drawer::new` (`palace.rs:316`). Not v7, no ordering |
| `room_id: Uuid` | yes | UUIDv5 over (wing_id, lowercased label), ADR-0027 (`docs/reference/shared-memory-identity.md` Decision 7) |
| `content` | yes | private; set via `set_content` |
| `importance: f32` | yes | 0.5 for remember, 1.0 for note |
| `source_file` | yes | local path, optional |
| `created_at` | yes (ms) | `Utc::now()` at write |
| `tags: Vec<String>` | yes | free-form plus `creator:*`, `ws:` |
| `drawer_type` | yes (string) | `UserFact`, `SessionEvent`, `AgentNote`, `Commit`, `Unknown`, `Task` (`palace.rs:122-149`) |
| `expires_at` | yes (ms) | `SessionEvent` default 7 days (`palace.rs` `with_type`) |
| `completed_at` | yes (ms) | Task only |
| `fact_key` | yes | Tier C slot; index `DRAWERS_BY_FACT_KEY` (`kg_store.rs:92`) |
| `last_accessed_at`, `access_count` | NO | in-memory only (not in `drawer_to_record`) |
| `content_hash` | NO, derived | SHA-256 over normalized body, recomputed at every load (`palace.rs:240-270`) |

- No `updated_at`, no revision id, no tombstone, no author/machine field. VERIFIED by the struct and `DrawerRecord` field lists above. `docs/reference/shared-memory-identity.md` Decision 6 states the same: "`Palace`, `Wing`, `Room`, and `Drawer` carry no provenance at all".
- Content hash: VERIFIED `tc` `memory_core/content_hash.rs:49` `CONTENT_HASH_VERSION = 1`, `:195` `normalize_for_hash`, `:252` `memory_content_hash`. Body only. Version is folded into the preimage. The hash changes if the body changes, and the dream cycle rewrites bodies in place (`docs/reference/shared-memory-identity.md` Decision 1, 2).
- Tier C / ADR-0028: VERIFIED `tc` `retrieval/tier_c.rs:45` default TTL 24 h, `:53` key max 128 bytes. Writing an occupied `fact_key` retires the incumbent in the same redb transaction (`tier_c.rs:1-35`). A Tier C drawer is `fact_key.is_some()` (`palace.rs:451`).
- Creator attribution (DOC-53): VERIFIED prefixes `creator:client=`, `creator:version=`, `creator:source=`, `creator:cwd=`, `creator:session=`, `creator:workstream=` and `ws:` (`crates/trusty-memory/src/attribution.rs:40-108`). They are ordinary tags, not a schema field. `creator:cwd=` carries an absolute local path, so it is path-bearing metadata that would cross machines in a tag-copying sync.
- Wing/room (ADR-0027): VERIFIED `RememberOptions.wing_id` (`tc` `retrieval/types.rs:150`); room resolved by label (`write_pipeline.rs:263-264`).
- Triples: VERIFIED `tc` `store/kg/types.rs:104-113`: `valid_from`, `valid_to`, `confidence`, `provenance`. Triples are temporal (closing sets `valid_to`) but drawers are not.

### 3b. Deletion
- VERIFIED `memory_forget` is a hard delete with no tombstone. `PalaceHandle::forget` (`tc` `retrieval/handle.rs:870-945`): deletes the redb drawer row (`:908`), the vector (`:923`), cascades KG triples with subject `drawer:<id>` (`:932`), removes the in-memory row (`:938`), rewrites the L1 snapshot. Then the tool layer deletes the BM25 doc (`memory_ops.rs:389-420`).
- VERIFIED the only durable deletion trail is `<palace data_dir>/maintenance_deletions.jsonl` (`tc` `memory_core/maintenance_log.rs:1-30`), and it covers maintenance deletions only. User forgets (`memory_forget`, drawer delete) "never reach this module" (`:19-20`); they get an `info` log line (`memory_ops.rs:429-436`, #8729). So a sync connector cannot learn about a user forget from stored state.
- Supersession: VERIFIED `superseded_by` triple `drawer:<orig> -> drawer:<canonical>` (`tc` `share/supersede.rs:41`). Used by the dream cycle. No tombstone field was added (Decision 5 of the identity doc).
- Expiry: VERIFIED `Drawer::is_expired_at` (`palace.rs:426`) is enforced at read time, and a purge runs on palace open when the process holds the maintenance lease (`registry.rs:333-335`).

### 3c. Palace id (ADR-0051)
- VERIFIED `PalaceId(pub String)` (`palace.rs:24`). Id is also the directory name (`docs/adr/0051...` Context; `registry.rs` `create_palace` joins `data_root` with the id). Valid ids: lowercase ASCII letters, digits, hyphens, letter or digit first, bounded length (`tc` `palace_id.rs:94-98`).
- VERIFIED NOT implemented: `Palace` is `{id, name, description, created_at, data_dir}` (`palace.rs:44-50`), with no `owner`/`project` fields and no `#[non_exhaustive]`. ADR-0051 is marked Accepted and says the fields are added. The only `owner: Option<String>` hit in `tc` `store/rooms.rs:392` is a test struct. Metadata persists in `<data_dir>/palace.json` (`store/palace_store.rs:20,122-160`).

## 4. Project identity

- VERIFIED palace resolution, highest first (`tc` `palace_resolve.rs:146-155` `PalaceSource`, `:398-430` `resolve_palace`): (1) `TRUSTY_MEMORY_PALACE` env, (2) committed pin `.trusty-tools/trusty-memory.yaml` (`PIN_FILE_REL`, schema `{schema_version, palace, note}`), (3) git `owner/repo` slug from `remote.origin.url` joined with a hyphen (`palace_id.rs:197`), (4) `parent-dir` slug of the main worktree root. A worktree and its main checkout resolve to the same palace (`palace_resolve.rs:1-30`).
- VERIFIED this repo's own pin: `.trusty-tools/trusty-memory.yaml` pins `palace: trusty-tools`, which overrides the git-derived `bobmatnyc-trusty-tools`. So the palace id differs from the git slug in this very repo, and two machines without the pin file would derive different ids.
- VERIFIED there is NO stable project id independent of path or slug in trusty-memory: no palace GUID (`grep guid` in `palace.rs`, `palace_store.rs`, `service/core.rs` returned nothing). `docs/specs/trusty-memory-chat-session-manager.md:467-469` says epic #1191 "has no code presence yet (identity remains slug-based)".
- Candidates that exist elsewhere in trusty-common (VERIFIED they exist; not used by trusty-memory):
  - `tc` `repo_identity.rs:1-40` `RepoIdentity` = `GitHub(owner/repo)` or `ContentHash(root-commit sha)` with a reversible canonical string (`content:<sha>`). It is a grouping key used by trusty-search.
  - `tc` `project_index_id.rs:1-50` `ProjectIdentity` {origin, root, operator}. It includes a hashed local root path, so it is NOT machine-portable.
  - `tc` `palace_alias.rs` aliases (`alias_target_if_absent`, used `tools/mod.rs:182`).
- ADR-0012 (search index GUID in a marker file) is for trusty-search only (`docs/adr/0012...:1-30`); it explicitly leaves trusty-memory slug-based.

## 5. Secret filtering and content gates on write (#2520)

- VERIFIED order in the MCP path (`crates/trusty-memory/src/tools/memory_ops.rs:36-190`): blocklist gate (`:78`, skipped when `force`), content gate (short content, `:92`), tag attribution, per-palace write lock (`:132`), dedup window gate (5 minutes, skipped when `force`, `:142`), tier C admission (`:165`), then the pipeline.
- VERIFIED secret gate in the pipeline (`tc` `retrieval/write_pipeline.rs:229-254`): without `force`, `FilterConfig::apply` runs noise patterns, then `check_secret`, then token and alpha-ratio checks. With `force` and without `allow_secret_like`, `check_secret` still runs on its own (`write_pipeline.rs:244-254`). Only `allow_secret_like: true` skips it. Detector: `tc` `memory_core/filter/secret.rs:96` `check_secret` -> `find_secret_token`.
- Where it does NOT run (VERIFIED):
  - `share::import` `insert_new` never calls `check_secret` or the quality filter (`share/import.rs:418-470`; comment at `:401-406` says the secret gate "must NOT" be skipped but defers it to a later PR). `export_palace_records` also does not screen (`share/export.rs:40-46`). `docs/reference/shared-memory-identity.md` ("Open: the export path can carry secrets") states the gap.
  - Drawers already stored before a filter fix are never re-screened. `trusty-memory audit secrets --count-only` measures them (`crates/trusty-memory/src/commands/audit_secrets.rs:1-25`).
  - `kg_assert`, `kg_bootstrap`, chat turns: not examined here.
- Would imported content pass through the gates? Via `memory_remember` over RPC: yes, all gates apply, including the dedup window and short-content drop, unless `force=true` (which still keeps the secret gate). Via `share::import`: no gate at all. Via `kuzu import`: the secret gate applies (`kuzu_import/screen.rs:1-25` plus the `remember_with_options` path), quality gates are off (`force: true`, `enforce_min_tokens: false`, `apply.rs:457-458`).
- INFERENCE: a pull path that wants "remote content passes the local secret gate" must call `check_secret` itself or route through `remember_with_options`; `share::import` would need a screen added.

## 6. Existing sync-like code

| Item | Status | What it syncs | Reusable for memory sync? |
|---|---|---|---|
| `tc` `memory_core/share` (#5902) + `docs/reference/shared-memory-identity.md` | Code merged, unwired | JSONL `SharedMemoryRecord` (content_hash, body, tags, created_at, drawer_type, room label, importance, versions; `share/record.rs:66-98`). Excludes expired and Tier C drawers (`share/export.rs:61-62`). Import: idempotent by hash, earliest `created_at` wins, tags union, importance max (`share/import.rs:304-390`). `merge_records` merges sets (`import.rs:479`) | Yes, closest fit. Gaps: no `fact_key`/wing/`expires_at` (import sets `expires_at=None`, `fact_key=None`, `import.rs:434-435`, default wing only), no deletes, no revision of edited memories (new body = new hash, old one only linked via `superseded_by`), no secret screen, no BM25/event side effects, no RPC entry point, needs embedder at import |
| ADR-0022 (knowledge-tree sync) | Accepted, deferred | Decides agent config goes to a monorepo and OKG knowledge trees to separate per-store repos; adds optional `sync_remote` on a `[[stores]]` binding. Does not cover trusty-memory palaces | No code. Only a precedent: separate repo per store, content class separation |
| ADR-0054 (`/tm-session-commit`) | Status Proposed. `grep` finds no implementation under `crates/` (only a mention in `share/mod.rs` and `worktree_reclaim_landed.rs`) | Plumbing commit to fixed branch `trusty/session-sync`, paths only under `.trusty-memories/sessions/<machine-id>/<session-id>/`, pre-commit secret scan, pull via `fetch` + `cat-file` | Design ideas only: machine-namespaced paths to avoid conflicts, secret gate before git object, read objects without checkout. Names `.trusty-memories/` as the future memory layout |
| ADR-0062 (session history as git refs) | Accepted and implemented: `crates/trusty-mpm/src/core/session_ref_publish.rs`, `tc` `catchup/session_refs.rs` | One orphan append-only ref per session `refs/tm/sessions/<user-id>/<session-key>`, lease-checked push, dedicated refspec, pre-push credential scan; push access is the trust boundary | Real, working git-ref transport with secret scan and lease push. Per-writer refs avoid merges. Retention and who-may-read are explicitly undecided (`ADR-0062` decision 8) |
| DOC-56 `docs/specs/trusty-agents-agents-sync.md` | Draft. `grep` for `tagent sync` / `sync_remote` finds no code | Agent config in private repo `bobmatnyc/trusty-agents-agents`. Rules: pre-push scrub gate that blocks and never redacts (S1-S4, `:235-250`), no force-push, no auto-resolve (D1-D4, `:383-397`), debounced push plus 15-minute pull floor, never merge into a running agent mid-turn (A1, `:421-436`), per-machine vs shared table (`:457-468`) | Policy only: fail-closed secret gate, no auto-resolve, cadence defaults. No code |
| `docs/specs/trusty-memory-chat-session-manager.md` (spec-001) | Draft; chat tools were then built (`tools/mod.rs:245-251`) | Chat turns and task drawers for one app, locally. Non-goal: "Real-time sync" (`:21`). Calls itself a sub-feature of #1683 (`:458-459`) | No. Only confirms #1683 (shared-memory service) is the open umbrella spec |
| `trusty-agents` memories export/import | Retired (#7360) | Was JSONL keyed `imported:{machine_id}:{id}`, so it could not converge | No; the identity doc explains why it was replaced |
| `tc` `residency.rs` | In use | trusty-mpm publishes an active-project set; memory/search pull it on a ticker with a freshness state machine (`:1-45`) | Pattern for a pull ticker with staleness handling |

## 7. Remote or HTTP API surface

- VERIFIED trusty-memory: none. ADR-0032 removed HTTP; the daemon serves only the UDS (`transport/uds.rs:1-20`). Wire contract: JSON-RPC over newline frames. A tool manifest exists as OpenRPC: `crates/trusty-memory/src/openrpc.rs:1-60` with scopes `memory.read`, `memory.write`, `knowledge.write`. `tools/mod.rs`/`transport/rpc.rs` is the dispatcher (about 75 names). Multi-tenant authz is a stub (`authz.rs:1-25`, `TRUSTY_MEMORY_MULTI_TENANT`), which only blocks `palace_create force=true`.
- VERIFIED ADR-0032: `trusty-console` is the only HTTP surface for the workspace (`docs/adr/0032-...:1-25`). Routes: `crates/trusty-console/src/server/router.rs:90-190`. ADR-0034 shows console receiving external HTTP and relaying over UDS to a supervised process. Console has a `memory_uds` module (`crates/trusty-console/src/memory_uds/`) that talks to the memory daemon.
- VERIFIED trusty-mpm daemon has its own axum HTTP API plus an RPC socket: `crates/trusty-mpm/src/daemon/mod.rs:136-141` (TCP bind plus `socket::bind`), routes `daemon/api.rs:200-236` (`/health`, `/sessions...`, `/projects...`, `/api/v1/projects/{name}/status`). No memory-sync route.
- Where specs live:
  - Workspace behaviour-contract specs: `docs/specs/` with the `DOC-N` / `SPEC-*~draft` anchors and SLD linting (`docs/specs/README.md:1-30`, `scripts/check_sld.sh`). Remote-memory spec #1683 is referenced from `docs/specs/trusty-memory-chat-session-manager.md:458` and `docs/reference/shared-memory-identity.md:8-10` but I found no spec file for it in the repo.
  - trusty-mpm specs: `docs/trusty-mpm/spec/` (ARCHITECTURE.md, COMPONENTS.md, PRD.md, SESSION_MANAGER_*.md) and `docs/trusty-mpm/design/RFC-*.md`; decisions in `docs/trusty-mpm/decisions/`. ADRs: `docs/adr/` with `INDEX.md`.
  - There is no OpenAPI file in the repo (`find` found only OpenRPC descriptions: `crates/trusty-memory/src/openrpc.rs`, `crates/trusty-mcp/src/openrpc.rs`, `crates/trusty-gworkspace/src/openrpc.rs`, and `docs/trusty-agents/research/openrpc-trusty-contract.md`).

## 8. Status and freshness surfaces

- `palace_info` (MCP tool, `crates/trusty-memory/src/tools/palace_ops.rs:229-267`): returns `id`, `name`, `drawer_count`, `room_count`, `wing_count`, `data_dir`, `last_used_unix`. Additive JSON, so a new key such as `sync` is a non-breaking addition.
- `console_metrics` (`crates/trusty-memory/src/console_metrics/mod.rs`): `ConsoleMetricsReport` (`tc` `console_metrics/mod.rs:93-113`) with an opaque `metrics` Value and `metrics_schema_version` (currently 5, `console_metrics/mod.rs:218-223`). Per-palace entries are built by `palace_entry` (`:440-495`): `drawer_count`, `vector_count`, `room_count`, `kg_triple_count`, `cached`, `stats_source`, `last_used_unix`, `disk_bytes`. Pattern: add per-palace keys and bump the schema version (the 3->4 and 4->5 bumps were additive).
- `memory.status` (`transport/methods/palaces.rs:37`), `memory.palaces_list` (`:188`), `memory.dream_status` and `memory.palace_dream_status` (`kg.rs:313-330`): aggregate and per-palace status RPCs. The dream status pair is the nearest model for a "background worker status per palace" method.
- Per-palace sidecar precedent: `crates/trusty-memory/src/palace_last_used.rs:1-40` stores `<data_dir>/last_used` (epoch seconds, throttled to one write per 60 s, deliberately not in `palace.json`). The doc gives the reason: `palace.json` is rewritten wholesale by `palace_update`. A sync-state file (last pull, last push, remote revision cursor) would follow that pattern.
- Console consumer: `crates/trusty-console/src/memory_uds/` polls the memory daemon over UDS (the dashboard polls `memory.status` and `memory.activity`, `transport/uds.rs:33-37`).
- Daemon health: `memory.health` (`METHOD_HEALTH`, `uds.rs:91`) and `transport/methods/health.rs`.

## 9. Design-relevant observations (INFERENCE unless a citation is given)

1. A connector that imports through the daemon's own write path must run in-process, or the daemon needs a new import RPC. A separate process cannot open the palace for writing (flock, Q2b). VERIFIED for the flock; the rest follows.
2. The dream cycle dedups and rewrites bodies (`dreamer.rs:182-200`, VERIFIED description). A pulled drawer can be merged away or rewritten locally. A body rewrite changes the content hash, so the same fact would look new on the next pull. The identity doc covers this via the `superseded_by` triple (Decision 1, 5), but nothing in code sends that triple across machines.
3. No user-forget record exists in stored state (Q3b), so a delete cannot sync without adding a tombstone or an append-only journal.
4. Tier C drawers and expired drawers are excluded from `share::export` by design (`share/export.rs:61-62`), so `fact_key` memories would not sync under the existing primitive.
5. The palace id is not a portable key (the pin file can override the git slug, Q4). A sync design needs a project key from `RepoIdentity` (`tc` `repo_identity.rs`) or a new field.
6. `creator:cwd=` and `ws:` tags are machine-local metadata inside the shared tag list.
