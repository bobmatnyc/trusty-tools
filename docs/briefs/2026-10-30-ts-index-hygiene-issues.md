# Issues: trusty-search index hygiene (filed as epic #10529: D1 #10530 … D7 #10536)

Design: [2026-10-30-ts-index-hygiene-design.md](2026-10-30-ts-index-hygiene-design.md). Rungs are from CLAUDE.md's Rust Test Ladder. Search keys used for every dedupe line: index, dedupe, duplicate index, auto-discover, stale, evict, park, project registry, index_status, residency, project_register.

Merge order: D1, then D2 and D3 in parallel; D4 after D1; D5 anytime; D6 after D5; D7 last.

---

## D1. feat(trusty-common): derive one index id from a stored project key

- **Body:** Add `project_key` to the tm registry record and one `derive_registry_index_id(name, project_key)` in trusty-common. Point `ensure_project_indexed_reporting` (`search_index.rs:386`) and the residency producer (`residency.rs:157`) at it. Design: [section 1](2026-10-30-ts-index-hygiene-design.md#1-id-derivation).
- **Closure:**
  - [ ] Registry record stores `project_key` (`owner/repo` or `local:<uuid>`) once; `mv`, repo rename and `git remote remove` leave it unchanged (test for each).
  - [ ] One function yields `<slug(name)>-<8 hex>`; the three call sites use it; a test asserts the id tm publishes equals the id registration creates.
  - [ ] Two registry projects with the same key are refused at register.
  - [ ] Legacy `derive_index_id` stays only for resolving existing ids; a doc comment says so.
  - [ ] Changelog fragments for `trusty-common` and `trusty-mpm`.
- **Rung:** 4 (public API and shared library; run each direct dependent).
- **Crates:** `trusty-common`, `trusty-mpm`, `trusty-code` (calls `ensure_project_indexed`).
- **Depends on:** none.
- **Dedupe:** overlaps #9169 (closed; built the map and resolver, did not derive ids) and #4269 (closed; documented the drift this fixes). Disposition: NEW; reference both, no reopen. Searched: index id derive, project registry.

## D2. feat(trusty-search): search.project.ensure creates an index when tm registers a project

- **Body:** New UDS RPC `search.project.ensure(project_key, name, root, profile)`: find-or-create, pin, queue the first reindex, idempotent. `project_register` calls it best effort and never blocks or fails on it. Session launch uses the same call. Design: [section 3](2026-10-30-ts-index-hygiene-design.md#3-lifecycle-state-machine).
- **Closure:**
  - [ ] Registering a project with a checkout on disk produces one pinned index with a derived id within one reindex cycle (live check on the sandbox daemon, `scripts/sandbox_search_daemon.sh`).
  - [ ] A second call returns the same id and queues no second reindex.
  - [ ] Daemon down: register succeeds, `tm doctor` reports `search_project_no_index`.
  - [ ] Ancestor or descendant of a registered root is refused with the overlapping id (the #6866 criterion 1 guard, still absent: `cto-assistant` sits inside `cto`).
  - [ ] MCP schema and `openrpc.rs` entries added.
- **Rung:** 6 (RPC and MCP surface), with rung 5 failure-path tests for the overlap refusal.
- **Crates:** `trusty-search`, `trusty-mpm`, `trusty-common`.
- **Depends on:** D1.
- **Dedupe:** overlaps #6866 (closed by fold into #6802, criterion 1 never built) and #5069 (closed; base facet registration at launch). Disposition: COMMENT on #6866 noting criterion 1 moves here; no reopen. Searched: project_register index create (no open hit).

## D3. feat(trusty-search): std-v1 collection profile (code, docs, excluded records)

- **Body:** Name the `std-v1` exclude profile and the `code` and `docs` path classes, store the resolved profile and hash on each index, apply it at create and through `PATCH /indexes/:id/config`. First target: the Architect index (`local/owner-queue/**`, archives, logs). Design: [section 2](2026-10-30-ts-index-hygiene-design.md#2-collection-taxonomy).
- **Closure:**
  - [ ] Creating an index with no explicit excludes applies `std-v1`; a test asserts an `owner-queue/items/x.json` fixture yields zero chunks.
  - [ ] Order of precedence tested: `.trusty-search.yaml`, then registry override, then `std-v1`.
  - [ ] Patching a profile removes newly excluded prefixes via `search.index.files.remove` and does not rebuild the corpus.
  - [ ] `index_status` shows `profile: std-v1@<hash>`.
  - [ ] Glob list confirmed against a `du` pass over `rdash`, `apex`, `cto` (design open question 6) and recorded in the PR.
- **Rung:** 3 (one crate); add rung 6 smoke if `index_status` fields change the MCP schema.
- **Crates:** `trusty-search`.
- **Depends on:** none (D2 passes the profile name; D3 works without it).
- **Dedupe:** overlaps #8922 (closed; exclude and sops files through the watcher), #9059 (closed; invalid globs hold the index), #7379 (closed; watcher honours filters). Disposition: reference only; they built the mechanism, this names the standard set. Searched: exclude_globs, collections config.

## D4. feat(trusty-search): drive residency from tm's active-project set

- **Body:** Add a pull ticker that calls `mpm.residency.active`, wraps it in `ResidencySnapshot`, pins the registered index ids, pre-wakes parked members, and parks non-members after the grace period. The recency cap then ranks only non-pinned indexes. Stale or missing set means no evictions. Design: [section 3](2026-10-30-ts-index-hygiene-design.md#3-lifecycle-state-machine).
- **Closure:**
  - [ ] An index in the active set is never chosen by `ids_to_park` (test beside `residency_sweep_tests`).
  - [ ] An index leaves residency only after the grace period with no query.
  - [ ] Stale or absent set: sweep behaves exactly as today (test with an injected clock).
  - [ ] A `Running` reindex still blocks the park.
  - [ ] Live check: open three projects in tm, confirm all three stay resident while an unrelated index is woken past the cap.
- **Rung:** 5 (process lifecycle, cross-crate contract; include `code-critic` round).
- **Crates:** `trusty-search`, `trusty-common`, `trusty-mpm` (only if the published ids need a field).
- **Depends on:** D1 (ids must match).
- **Dedupe:** overlaps #7087 (closed; search part was never built, "remainder is 1.x"; memory part folded into #9141) and #6821 (closed; the tier-scaled cap). Disposition: COMMENT on #7087 pointing at this draft, then file NEW since #7087 is closed as folded. Searched: residency active-project set search consumer (no open hit).

## D5. feat(trusty-search): per-index freshness and duplicate classification

- **Body:** Compute `freshness` (fresh, behind, unwatched, never_indexed, root_missing, unknown) and `duplicate_class` (worktree, orphan, same_root, nested_root, content_twin) from the persisted registry and resident handles, with no corpus load. Queue a bounded incremental reindex for `behind`. Extend the orphan reaper to parked rows. Design: [section 4](2026-10-30-ts-index-hygiene-design.md#4-stale-and-duplicate-detection).
- **Closure:**
  - [ ] Each freshness value has a test; a failed git probe yields `unknown`, never `fresh`.
  - [ ] Duplicate classifier reproduces the live table: a fixture with a worktree root, a missing root, a nested root and a same-content pair classifies each correctly.
  - [ ] Winner rule unchanged from f7 (main checkout, else most recent).
  - [ ] Parked orphan whose parent exists is evicted after the grace; one whose parent is missing (unmounted volume) is kept.
  - [ ] At most one auto-reindex per index per 10 minutes, none while one runs.
  - [ ] Content digest vs head SHA decision recorded (design open question 5).
- **Rung:** 5 (persistence and process lifecycle).
- **Crates:** `trusty-search`.
- **Depends on:** none for classification; D4 for the evict clock.
- **Dedupe:** overlaps #6866 (criteria 2, 3 and 5: auto park by age, reclaim of dead roots, counts; closed by fold, not built), #4846 (closed; warm boot walks dead entries), #8166 (closed; digest signal). Disposition: COMMENT on #6866 and #8166; NEW for the freshness field. Searched: stale index, orphan, reclaim.

## D6. feat(trusty-search): search.project.health RPC; parked-safe index_status; tm renders it

- **Body:** New read-only `search.project.health` returning the per-index row in [section 5](2026-10-30-ts-index-hygiene-design.md#5-health-contract-to-tm). `/health` and `console_metrics` gain counts. `index_status` answers a parked index with `lifecycle: parked` and persisted facts instead of the not-resident error. tm adds the doctor rows and a 60 s cached read for `project_list`.
- **Closure:**
  - [ ] RPC loads no corpus (test asserts resident count is unchanged after the call).
  - [ ] `index_status` on a parked index returns 200 with lifecycle and last_indexed; it does not wake the index.
  - [ ] `/health` exposes `registered`, `resident`, `parked`, `orphaned`, `duplicate_groups`, `behind`, `never_queried`, `hand_named`.
  - [ ] `tm doctor` shows `search_index_stale`, `search_index_hand_named`, `search_index_nested_root`, `search_index_orphan_parked`; an unreadable answer is "not checked".
  - [ ] Daemon unreachable: tm degrades to "not checked" within the existing probe timeout.
  - [ ] One binary smoke run on the sandbox daemons.
- **Rung:** 6 (API surface), Rust part at rung 4.
- **Crates:** `trusty-search`, `trusty-mpm`, `trusty-common` (console_metrics report), possibly `trusty-console`.
- **Depends on:** D5 (fields); D1 for `project_key` and `id_origin`.
- **Dedupe:** overlaps #5061 (closed; `index_status` 404 for parked, fixed as an explicit 503 in #8687), #6866 criterion 5 (counts), #4315 (open; trusty-agents per-connection health API, different consumer). Disposition: COMMENT on #4315 only if its scope overlaps the RPC; NEW otherwise. Searched: index_status health tm.

## D7. feat(trusty-mpm): tm project adopt-indexes and doctor --fix evicts classified duplicates

- **Body:** Migration command from [section 6](2026-10-30-ts-index-hygiene-design.md#6-migration): backfill keys, pin existing indexes in place (`id_origin = legacy`), print the duplicate table, `--apply` parks, `--evict-now` removes orphans whose parent exists. Extends the closed `--fix` that only parks. Calls daemon RPCs; never writes the map file.
- **Closure:**
  - [ ] Dry-run prints key, index id, class and action for every registry project and every unmapped index; writes nothing.
  - [ ] `--apply` parks only rows the classifier flagged; refuses when the map is unreadable (same posture as #9664).
  - [ ] No id is renamed; resolve-by-name works before and after (test).
  - [ ] Eviction logs the corpus bytes removed; `assistant-okg-*` indexes are skipped by the `owner` tag.
  - [ ] Live run on this host: the 12 orphaned and 9 worktree-rooted parked entries from the design brief are cleared or explained.
- **Rung:** 5 (persistence, irreversible delete; `code-critic` round).
- **Crates:** `trusty-mpm`, `trusty-search` (RPC for evict).
- **Depends on:** D1, D5. D6 recommended for the post-run check.
- **Dedupe:** overlaps #9664 (closed; `--fix` parks duplicates) and #8687 (closed; index remove aborts on cold-parked 503). Disposition: reference; NEW for evict and adoption. Searched: tm doctor park, duplicate index.

---

## Not drafted

- **Resident cap and warm list:** Bob decides (MB0bed rec 5); D4 reads `index.warm` once decided.
- **Architect-side items** (project key map, search recipes, daily audit line): Architect-owned in MB0bed, not trusty-tools work.
- **`search_all` fan-out under a pin:** MB0bed rec 8 asks Bob whether to file. Searched: fan-out, search_all, pin; no draft here.
