# trusty-search index hygiene under trusty-mpm: design brief

**Status:** Filed 2026-10-30 as epic #10529 (owner ruling D22); build in rank order D1 #10530 → D2 #10531 → D3 #10532 → D4 #10533 → D5 #10534 → D6 #10535 → D7 #10536. Drafts as filed: [2026-10-30-ts-index-hygiene-issues.md](2026-10-30-ts-index-hygiene-issues.md).

**Recommendation:** Make the tm project registry the single source of project keys, derive every index id from that stored key, let tm's active-project set drive park and evict, and report one health row per index back to tm over the existing UDS RPC.

- **Most plumbing exists; the wiring does not.** The project map, resolver, park RPC, `tm doctor` duplicate check and the `mpm.residency.active` producer all ship. Search never consumes the active set, so residency is recency-only ([anchor](#3-lifecycle-state-machine)).
- **Two id schemes disagree.** Registration uses the root basename (`search_index.rs:386`); tm publishes a different label-plus-digest id (`residency.rs:157`). Neither matches a registered id like `cto` ([anchor](#1-id-derivation)).
- **One index per project, two collections inside it.** `code` and `docs` are path classes in one index; records, dashboards and archives are excluded by a standard profile ([anchor](#2-collection-taxonomy)).
- **Live state is unhygienic.** 84 registered indexes: 10 resident, 74 parked; 12 parked roots are orphaned, 9 are worktrees, 1 root is nested inside another ([anchor](#4-stale-and-duplicate-detection)).
- **Health goes tm-ward as a pull, not a push.** New `search.project.health` over UDS carries per-index rows; `/health` carries counts only ([anchor](#5-health-contract-to-tm)). Migration adopts hand-named ids in place, with no rename ([anchor](#6-migration)); decisions for Bob are in [open questions](#7-open-questions).

---

## 0. What already exists (so slices stay small)

| Capability | Where | Gap |
|---|---|---|
| Canonical project to index map, pins, parked rows | `crates/trusty-search/src/core/project_map.rs:1-80`, RPCs `search.project.{map,resolve,pin,park,unpark}` | Keys on repo identity or `root:<path>`; no tm project key |
| Resolver: main checkout wins, worktree never wins, duplicates listed | `service/project_resolve.rs:1-20` | Reports `duplicates`; nothing acts on them except `project park` |
| `tm doctor` rows: no index, duplicate, two checkouts, map unreadable | `crates/trusty-mpm/src/daemon/doctor_search_project.rs:1-40` | Read-only; `--fix` parks (closed #9664) but never evicts |
| Active-project producer `mpm.residency.active` | `crates/trusty-mpm/src/daemon/managed_routes/residency.rs:1-30` | No consumer in trusty-search (grep for `ActiveProjectSet` in `crates/trusty-search/src` outside tests: no hit) |
| Resident cap, recency sweep, age gate | `service/lazy_loader/residency.rs:285`, `service/server/residency_sweep.rs`, `lazy_loader/store.rs:70-100` | Ranks by recency; ignores which projects are active |
| Orphan reaper | `service/orphan_reaper.rs:1-30` | Doc says it unregisters live indexes; 12 orphaned parked entries remain (hypothesis: parked rows are skipped, not verified) |
| Create at session launch, base facet, worktree index | `core/session_launch/search_index.rs:70`, `core/base_facet_index.rs:167`, `core/worktree_index.rs:243` | `project_register` itself creates no index (`mcp/project_dispatch.rs:58-85`) |
| Per-index excludes, `include_docs`, `extra_skip_dirs` | `service/persistence.rs:35-70`, `core/project_config.rs`, `core/repo_config.rs:40-60` | No named standard profile; each project sets its own or none |
| Boot and wake reconcile (git delta, mtime walk) | `service/reconcile.rs:1-50` | Result is not exposed as a per-index freshness field |
| Console metrics | `mcp/tools/misc.rs:228` | Lists id, root, size, last_used; no lifecycle or freshness |

---

## 1. Id derivation

**Problem.** Three derivations coexist.

1. `derive_index_id` returns the root basename (`crates/trusty-common/src/index_id.rs:103`). `ensure_project_indexed_reporting` registers it (`search_index.rs:386`). A basename collision falls back to a hash suffix (`search_index_reconcile.rs:273`). That is where `trusty-tools-4e2cf878` and `apex-9a4a584b` come from.
2. `ProjectIdentity::index_id` returns `<owner-repo>-<16 hex>` over origin, root and operator (`project_index_id.rs:266`). tm publishes this in the active set (`residency.rs:157`). It matches no registered id, so a pin built from it would match nothing.
3. Operators hand-name via `.trusty-search.yaml` `name:` (`core/project_config.rs:45`) or `create_index id`.

Both derived forms depend on the root path or on live git state, so `mv`, a repo rename or `git remote remove` changes the id (documented at `project_index_id.rs:200-240`, #4269).

**Scheme.**

- `project_key`: assigned once when the project is registered in tm, then stored in the registry record (`crates/trusty-mpm/src/project/record.rs:24`). Value: the canonical `owner/repo` when an origin exists, else `local:<uuid>`. It never recomputes, so rename and `mv` do not change it. This matches the group key the map already uses (`project_resolve.rs:158`).
- `index_id = <slug(project.name)>-<first 8 hex of sha256("v2:" + project_key)>`. The label is cosmetic; the digest carries uniqueness. 8 hex matches the two existing suffixed ids.
- One function in `trusty-common` produces it. `ensure_project_indexed`, tm's residency producer and `search.project.ensure` all call it. The two older functions stay for resolving legacy ids only.
- Worktrees get no id. Owner ruling 2026-09-05 (#5069, quoted in #6866): an index is keyed by repo identity and serves the main checkout plus every worktree.
- Hand-named creation stays possible with an explicit flag (`--id`), and the map marks it `id_origin = "operator"`. Health reports such an index as `hand_named: true`.

**Bad inputs.** Two registry projects with the same `project_key` are refused at register. A project with no checkout on disk gets a key and no index until a root exists.

---

## 2. Collection taxonomy

**Decision.** One index per project (owner instruction 1), two collections as path classes inside it. Two indexes per project would double the resident count under a cap of about 10, which the live daemon already thrashes (section 4 evidence).

The existing `mode` argument (`code`, `text`, `data`) already ranks by file class. The change is to name the classes, fix their globs, and make the exclude profile standard.

| Collection | Includes | `mode` |
|---|---|---|
| `code` | Source by language the walker admits; config that ships with code (`Cargo.toml`, `package.json`, SQL migrations) | `code` |
| `docs` | `*.md *.mdx *.rst *.adoc *.txt` anywhere, ranked first under `docs/**`, `specs/**`, `**/adr/**`, `**/decisions/**`, `**/briefs/**`, `README*`, `CHANGELOG*` | `text` |
| `excluded` | Never indexed (list below) | none |

**Standard exclude profile `std-v1`** (globs matched on the index-relative path; applied through `exclude_globs`, which `create_index` already accepts and #8922 validates):

```
**/owner-queue/**        **/owner-queue-archive/**
**/archive/**            **/_archive/**
**/node_modules/**       **/.worktrees/**
**/*.db  **/*.sqlite*    **/*.lock
**/dashboard*.log        **/dashboard*/**/*.json
**/checkpoints/**        **/backups/**    **/logs/**
**/actions.md            **/usage/**
```

Rationale for each record class, from the Architect project: `local/owner-queue/items/*.json` makes a decision query return the answered item (brief MB0bed, "Self-contamination"); `local/actions.md` is 8.7 MB; `local/usage` is 31 MB; `local/briefs` is 1.6 GB on disk, so the profile must not exclude `briefs/**` but should exclude binary attachments (not measured: which file types fill it).

**Where the profile lives.** Per-project overrides sit in the tm registry record (`index.exclude`, `index.include_extra`) and travel in the `search.project.ensure` call. A committed `.trusty-search.yaml` still wins for a repo that has one, then the registry, then `std-v1`. Search stores the resolved profile name and a hash on the index, so health can show `profile: std-v1@3fa2` and flag drift.

**Changing a profile** triggers `search.index.files.remove` for newly excluded prefixes (exists, #10110), then a reindex. It does not rebuild the whole corpus.

---

## 3. Lifecycle state machine

```
          ensure(project)                active set / query            grace expired, not queried
 (none) ------------------> CREATED ------------------> ACTIVE <-----------------------------+
                               |  first reindex ok        |  ^                                |
                               +-------------------------->  |  wake (query, or enters       |
                                                          v  |  active set)                  |
                                                        PARKED ------------------------------+
                                                          |
                                      evict rules (below) v
                                                       EVICTED (tombstone row; corpus deleted)
```

| Transition | Trigger | Existing or new |
|---|---|---|
| none to CREATED | `project_register` calls `search.project.ensure` (best effort, async, never blocks register); also session launch (`search_index.rs:70`) through the same call | New call; launch path exists |
| CREATED to ACTIVE | First full reindex completes; lexical stage `ready` | Exists |
| ACTIVE to PARKED | Project absent from `mpm.residency.active` for the grace period AND no query in that period; or resident cap exceeded (active-set members are never the ones parked) | Cap sweep exists (`residency_sweep.rs`); active-set input is new |
| PARKED to ACTIVE | Query wakes it (60-250 ms observed in MB0bed); or project enters the active set (search pulls the set on its ticker and pre-wakes) | Query wake exists; pre-wake is new |
| PARKED to EVICTED | Any of: root missing and parent present for 7 days; worktree root; duplicate loser parked more than 30 days; project removed from the tm registry; no query for 90 days AND project not in the registry | Orphan reaper covers live rows only (hypothesis); rest is new |
| EVICTED to CREATED | `ensure` again | New |

**Never evicted:** an index whose project is in the tm registry with a present root. It parks, it does not die, because a 145k-chunk reindex is expensive and a parked wake is cheap. Parked indexes of registered projects cost disk only.

**Reindex triggers.**

1. Create and wake: git delta or mtime walk through `reconcile_one_index` (exists).
2. Watcher events on resident indexes (exists).
3. Stale detection says `behind` (section 4): queue an incremental reindex, at most one per index per 10 minutes, none while another runs.
4. tm calls `search.index.reindex` after it fast-forwards a main checkout (new call site in tm).
5. A once-daily sweep reindexes `behind` indexes of active projects only.

**Guards.** No transition runs while a reindex is `Running` (`residency_sweep.rs` already enforces this for park). Any evict refuses an index with dirty writes pending (#6870 closed that class for park). An unreadable map stops all evicts (same posture as #9664's `--fix`).

**Active set dependency.** `mpm.residency.active` fails closed on tmux errors (`residency.rs:20-25`). Search keeps the previous set while fresh and stops evicting when the set is stale (`ResidencySnapshot`, `trusty-common/src/residency.rs`). An empty or missing set means no evictions, only the existing cap sweep.

---

## 4. Stale and duplicate detection

### Stale

Per index, computed without loading the corpus:

| `freshness` | Rule |
|---|---|
| `fresh` | Git root: indexed head equals `git rev-parse HEAD` of the root. Non-git: no file newer than `last_indexed`. Watcher active or index parked with no change |
| `behind` | Head differs, or newest mtime exceeds `last_indexed`. Carries `behind_commits` when git can say |
| `unwatched` | Resident, root present, no active watcher |
| `never_indexed` | Zero chunks, or lexical stage not ready after the create grace |
| `root_missing` | Root absent (`orphan_report.rs` classifies present, orphaned, indeterminate) |
| `unknown` | Probe failed (git error, dubious ownership). Never reported as `fresh` |

Use a content digest, not `indexed_head_sha`, where #8166 landed one; check that issue's outcome before wiring (open question 5).

**Live evidence (list_indexes, 2026-10-30 about 13:45Z).** `apex-companion` last indexed 2026-10-29T18:31Z, about 19 h old, and in use. MB0bed section 4.5 already notes that a merge ruling on a day-old tip can be wrong. Nothing in `list_indexes` marks it stale.

### Duplicates

A group is all indexes sharing a group key (`project_resolve.rs:158`) or whose roots nest. Classes, in order:

1. `worktree`: root is a git worktree.
2. `orphan`: root missing.
3. `same_root`: two ids, one canonical root (`identifies_same_path`, `index_id.rs:217`).
4. `nested_root`: one root inside another. The #6866 overlap guard was meant to refuse this; its closure was a fold into #6802, not an implementation.
5. `content_twin`: two roots, same content hash (identity `content:<sha>`).
6. `same_repo_two_checkouts`: already `search_project_two_checkouts` in tm doctor.

Winner rule is unchanged (f7): main checkout, else most recently indexed. Losers get a `[[parked]]` row with a new `reason` and the winner id, and enter the 30-day evict clock.

**Live evidence.**

| Finding | Source | Class |
|---|---|---|
| 74 parked plus 10 resident = 84 registered | `list_indexes` | n/a |
| 12 parked entries `orphaned`: `MailWarden`, `adaptive-crm-wt`, `ai-code-review-web`, `ai-trackdown`, `aipowerranking`, `armature`, `claude-mpm-marketplace`, `mcp-memory`, `notion-mpm`, `qa-172-fixture-20260924`, `tm-duetto-agents-skills-01`, `tm-hackathon-01` | `list_indexes.parked[].root_state` | `orphan` |
| 4 parked `indeterminate`, all test or audit roots: `db58ea56-...`, `proj`, `repo-acme-test-bc91d18f`, `tiny-target-03cc4b2b` | same | `orphan` candidate |
| 9 parked indexes rooted in `.worktrees/`: `9cb1aa19-...`, `db58ea56-...`, `e99bcc7f-...`, `tm-apex-companion-01`, `tm-cto-reports-02`, `tm-duetto-agents-skills-01`, `tm-hackathon-01`, `tm-hotstats-cloud-01`, `tm-hotstats-product-poc-01` | same | `worktree` |
| `cto-assistant` root `.../bob-duetto/cto/projects` sits inside `cto` root `.../bob-duetto/cto` | `list_indexes` | `nested_root` |
| `apex` (root `/Users/masa/Duetto/repos/APEX`) and `apex-9a4a584b` (`duettoresearch/apex`) | `list_indexes` | probable `same_repo_two_checkouts`; `apex` identity not read because it is parked |
| `supervisor` has the same content hash as `architect`, 290 chunks | MB0bed section 1 | `content_twin`; not re-read here, `supervisor` is parked |
| `ml-pricing-engine` and `flyr-ml-pricing-engine` (different roots) | `list_indexes` | possible duplicate; not verified |
| Only 2 project ids carry a derived suffix (`trusty-tools-4e2cf878`, `apex-9a4a584b`); all other project ids are bare basenames | `list_indexes` | `hand_named` |
| The 10-slot resident set changed within about 15 minutes of MB0bed's inventory: `code-intelligence`, `mpm-utils`, `supervisor` left; `ai-power-rankings`, `mcp-services`, `apex-9a4a584b` entered | MB0bed section 1 vs `list_indexes` | recency thrash |
| Daemon total chunks 371,327 at 13:30Z vs 468,937 now | MB0bed vs `search_health` | n/a |

`index_status` on a parked index returns an error ("registered and built but is not currently resident"), so neither `apex` nor `supervisor` could be inspected. Parked state should answer with metadata, not an error (anchor 5).

`mcp__trusty-mpm__project_list` was not reachable from this session, so registry-versus-index coverage (projects with no index) is not measured. `tm doctor`'s `search_project_no_index` row measures it per project.

---

## 5. Health contract to tm

**Transport.** tm already calls trusty-search over the UDS RPC (`crates/trusty-mpm/src/daemon/search_rpc.rs`; `doctor_search_project.rs`). Keep one transport and one direction.

| Surface | Transport | Content | Consumer |
|---|---|---|---|
| `search.project.health` (new) | UDS RPC, read-only, bounded, loads no corpus | Per-index rows (below) | `tm doctor`, `tm status`, `project_list` enrichment (cached 60 s) |
| `/health` (extend) | HTTP/UDS existing | Counts only: `registered`, `resident`, `parked`, `orphaned`, `duplicate_groups`, `behind`, `never_queried`, `hand_named` | Console, launchd checks |
| `console_metrics` MCP (extend) | MCP | Same counts plus per-index `lifecycle`, `freshness` | trusty-console |
| `index_status` MCP/RPC (extend) | MCP/RPC | Parked indexes answer 200 with `lifecycle: parked` and persisted facts | Agents, Architect pack script |
| `mpm.residency.active` (exists) | UDS RPC, search pulls | Active project set with registered index ids | Search residency |

No push from search to tm. tm pulls on doctor, status and a 60 s cache; this avoids a second bus and matches the fail-closed pull search already owes `mpm.residency.active`.

**Row (`search.project.health` item).**

| Field | Type | Note |
|---|---|---|
| `index_id` | string | |
| `project_key` | string or null | Null for an unmapped legacy index |
| `project_name` | string or null | tm registry name |
| `id_origin` | `derived` / `legacy` / `operator` | `hand_named` is `id_origin != derived` |
| `lifecycle` | `created` / `active` / `parked` / `evicted` | |
| `resident` | bool | |
| `root_path`, `root_state` | string, `present` / `orphaned` / `indeterminate` | |
| `freshness` | enum from section 4 | plus `behind_commits` |
| `last_indexed_unix`, `last_used_unix` | int | |
| `chunk_count`, `vectors_present` | int | Coverage = `vectors_present / chunk_count` |
| `stages` | lexical / semantic / graph status | From `list_indexes` today |
| `watcher` | `active` / `none` / `degraded` | |
| `profile` | `std-v1@<hash>` | |
| `duplicate_of` | string or null, plus `duplicate_class` | |
| `park_reason` | enum or null | Existing `ParkReason` plus new classes |

**Verdicts tm renders.** Reuse `doctor_search_project.rs` rows and add `search_index_stale`, `search_index_hand_named`, `search_index_nested_root`, `search_index_orphan_parked`. Each is `Warn`. A row whose input could not be read is "not checked", never `Ok` (existing rule, `doctor_search_project.rs:15-16`).

---

## 6. Migration

Principle: adopt in place, never rename. A rename moves a 145k-chunk corpus (2.3 GB for `trusty-tools-4e2cf878`) and breaks every pinned `.mcp.json` and `TRUSTY_INDEX` env (ADR-0042).

1. **Backfill `project_key`.** A tm command (`tm project backfill-keys`, dry-run first) computes the key for each registry record from its checkout and stores it. Records with no checkout are listed, not guessed.
2. **Pin existing indexes.** For each registry project, resolve its index through the existing resolver, write a pin row with `id_origin = legacy` (or `derived` when the id already equals the new scheme). No id changes. Resolve-by-name keeps working (#9169).
3. **Classify the rest.** Run the duplicate classifier. Everything classed `worktree`, `orphan`, `nested_root`, `content_twin` becomes a `[[parked]]` row with a reason. Dry-run prints the table; `--apply` writes it. This extends the closed #9664 `--fix`, which parks only.
4. **Evict on the clock** from section 3, or immediately with `--evict-now` for orphans whose parent exists (the orphan reaper's own safety rule).
5. **Apply `std-v1`** to indexes of registered projects through `PATCH /indexes/:id/config`; newly excluded prefixes are removed with `search.index.files.remove`. The Architect index goes first because it holds the contamination.
6. **New projects** get a derived id at register. The first real test is the next `project_register`.

Rollback: parked rows and pins live in `project-map.toml`; `unpark` and `unpin` exist. Eviction is the only irreversible step, so it runs last and logs the corpus size removed.

Exempt class: the four `assistant-okg-*` indexes under `/Users/masa/trusty-agents/*/okg` are not projects. They carry `owner=agents` and the hygiene sweep skips them (open question 3).

---

## 7. Open questions

1. **Resident cap (Bob).** MB0bed already marks it Bob's call. With active-set pinning, the cap only needs to cover the active projects plus a small warm list. Proposed default: pinned active set plus 3 warm slots, tier-scaled upper bound as today (`residency.rs:108`). The embedder already holds about 1.65 GB RSS now, 3.5 GB at 13:30Z.
2. **Warm list for `apex`, `mcp-services`, `ai-power-rankings`, `cto-reports` (Bob).** MB0bed recommends it. This design treats the warm list as a registry field `index.warm = true` that the sweep honours. Cost: memory only.
3. **Non-project indexes** (`assistant-okg-*`, scratch). Exempt by an `owner` tag, or a separate registry? Proposed: tag.
4. **Evict horizon.** 90 days idle for unregistered indexes and 30 days for duplicate losers are proposals, not measurements. #6866 suggested 30 days for idle park. Bob confirms or changes.
5. **Freshness signal.** Confirm whether #8166 (closed) shipped a content digest that stale detection should use instead of the head SHA. Not read in this pass.
6. **Records exclusion beyond Architect.** `std-v1` is derived from the Architect tree and the brief. `rdash`, `apex` and `cto` were not scanned for record directories. A one-pass `du` per registered project should confirm the glob list before the profile ships.
7. **Docs collection granularity.** One index with path classes cannot rank `docs` above `code` for a mixed query without `mode`. If agents need both in one call, the current fan-out under a pin (MB0bed rec 8) interacts; not designed here.
8. **Worktree policy.** Ruling 2026-09-05 says worktrees register no index, yet `worktree_index.rs:243` still registers `skip_vector` indexes and 9 sit parked. Whether the design removes that code path or only parks its output needs an owner call.

Not checked: the content of `project-map.toml` on this host, the `tm` registry record count, `/health` live output (the `search_health` tool returns a summary only), and whether `supervisor` and `architect` still share content.
