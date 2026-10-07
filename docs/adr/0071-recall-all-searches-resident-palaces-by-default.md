# 0071. recall_all searches resident palaces by default and reports what it skipped

- **Status:** Accepted
- **Date:** 2026-10-07
- **Accepted:** 2026-10-07 (Bob, "Accept with my recs", 08:20Z)
- **Scope:** crates `trusty-memory` (all three recall_all surfaces),
  `trusty-common` (`memory_core`); consumer `trusty-agents`
- **Reversibility Cost:** Medium before 1.0.0, High after — the default scope
  and the completeness fields become frozen MCP behaviour under ADR-0066
- **Decision Drivers:** latency (#9141 AC1), no cold opens (#9299), RAM
  (#6802), ADR-0066 contract, no silent partial answers — see below
- **Supersedes / Superseded by:** — (replaces the "open every palace" rule
  recorded in code under #4637 and #7125; see Related Decisions)

## Context

Line references are at `origin/main` db8dbfef65. `memory_recall_all` lists
every palace on disk, drops provably empty ones, and cold-opens the rest.
The handler lists palaces
(`crates/trusty-memory/src/tools/recall_ops.rs:419`) and applies the #9141
empty skip (`:422`, `service/recall_stream.rs:133-166`). `recall_streamed`
then opens the remainder in batches of 8 (`recall_stream.rs:51`, `:96-108`),
four opens at a time (`service/helpers.rs:236`). After each batch it releases
every palace that was not resident when the call began (`recall_stream.rs:191`).

That release is why a "warm" call is not warm. Each call cold-opens every
non-resident, non-empty palace again. A cold open replays the HNSW graph
(332-468 ms) and runs the #9265 stranded-point scan (178-272 ms,
`trusty-common/src/memory_core/store/hnsw_store.rs:346`). Each hydrated
palace costs about 90 MB (`trusty-memory/src/lib.rs:130`).

The code rejects a resident-only answer on purpose. `helpers.rs:240-243`
records the #4637 rule: answering from cached palaces only "would silently
drop" most of the corpus. #7125 kept that rule and bounded only the peak.

Measured state:

- 0.29.0 (#9141, 2026-10-06): warm calls 52.9 s and 68.6 s; a cold first
  call passed 120 s while RSS rose from 0.54 GB to 5.4 GB.
- 0.29.3 (2026-10-07, loaded host, re-measure pending): 103 palaces, 42
  searched, 61 skipped, resident count 10 → 11, warm p95 about 2.9 s.
- Warm single-palace recall: 3-5 ms (#9299 comment, 2026-10-06).

Three defects sit beside the latency. `palaces_searched` reports palaces
attempted, not searched (`recall_ops.rs:474`). A palace that fails to open is
dropped with only a log line (`helpers.rs:299`). `MemoryService::recall_all`
and the chat tool return a bare array with no counts
(`service/core_recall.rs:103-141`, `chat/tools.rs:352`). So the tool can
already return a partial answer that looks complete.

## Decision Drivers

1. #9141 AC1: warm p95 under 1 s over distinct queries at 100+ palaces.
2. #9299 closure: serve from resident palaces without cold-opening the rest.
3. A partial answer must say it is partial, on every surface.
4. A recall must not grow residency or RAM (#6802, #7125).
5. ADR-0066: 1.x changes are additive; a default change must land in 0.x.
6. Results should be reproducible enough to test (#9141 AC3).

## Considered Options

**Option 1. Resident palaces only, rest reported as not searched.** Search
only palaces the registry holds when the call starts
(`PalaceRegistry::list`, `memory_core/registry.rs:427`). Open nothing.
Latency is R warm searches, about 3-5 ms each. RAM does not change. Cost:
coverage equals the resident set, 10-11 of 42 non-empty palaces in the
0.29.3 run. Results vary with residency. Adding "palaces opened in the last
N minutes" gains nothing: a palace used within the 300 s idle window
(`idle_evict.rs:82`) is still resident; one used earlier needs a cold open.

**Option 2. Persisted per-palace router (BM25 terms or a centroid).** Pick
candidate palaces from a small summary file, then open only the top k.
Coverage of the whole estate is preserved in principle. Cost: a new
artifact that every write must keep fresh, and that ADR-0067 must version
if it lives in the palace directory. The existing `bm25_index.json` is no
shortcut: loading it replays every document (`bm25_index.rs:1-23`). Routing
quality is unmeasured, and the vector lane already misses drawers (#9174).
The chosen palaces are still cold-opened, so latency is k cold opens.

**Option 3. Bounded parallel cold open with a time budget.** Open what fits
in, say, 800 ms and flag the result partial. Latency is bounded. Cost: which
palaces finish depends on host load, so results are not reproducible and AC3
cannot be tested. Every call still pays opens and a RAM spike of
concurrency × 90 MB. At the measured load of 7-42, few palaces would fit.

**Option 4. Caller-chosen scope.** Add an optional `scope` param
(`resident` | `all`) and optionally a `palaces: [...]` list. This is additive
under ADR-0066. On its own it does not choose the default, and the default is
what AC1 measures.

**Option 5. Make cold opens cheap.** Persist and mmap the HNSW graph
(#6833, #6835) and defer the stranded scan. This helps single-palace recall
after idle eviction (2.2-8.7 s live) as well. Cost: large engine and format
work, and each call stays O(non-resident palaces). At 31 palaces, 100 ms per
open and 4 concurrent opens, a call still takes about 0.8 s.

## Decision

Accepted by Bob on 2026-10-07. We will adopt Option 1 as the default and
Option 4's `scope: "all"` as the explicit full search, with a completeness
report on every response. Option 5 stays separate work under #6802.

**Owner rulings (Bob, 2026-10-07):**

1. **Partial default accepted.** The default searches resident palaces only,
   and coverage is always reported (D4). This reverses the
   [#4637](https://github.com/bobmatnyc/trusty-tools/issues/4637) rule that a
   cross-palace recall must open every palace.
2. **`scope: "all"` stays unbounded.** It has no time budget; it reports
   coverage like the default.
3. **recall_all must not reset a palace's idle clock** (D2).
4. **`palaces: [...]` is deferred.** It ships later as an additive 1.x param.
5. **#9141 AC3 is restated** as: "`scope: "all"` returns the same top 5 as
   before the fix for a fixed query; the default-scope top 5 equals the
   `scope: "all"` top 5 restricted to resident palaces."

**D1. Default scope is resident.** With no `scope`, `memory_recall_all`
searches the palaces the registry holds at call start, minus provably empty
ones. It opens no palace and does not change the resident set.

**D2. Residency policy.** "Resident" is what the registry holds; recall_all
never decides it. The policy is the one #9141 inherited from #7087: active
session palaces and `default_palace` are pinned; any palace touched by a
targeted call stays resident for the 300 s idle window; the LRU cap bounds
the rest (`registry.rs:71`). A recall_all search must not reset a palace's
idle clock. Today `recall_scoped` and `recall_deep_scoped` call `touch()`
(`memory_core/retrieval/layers.rs:642`, `:736`). Without this rule, a
recall_all every few minutes would keep the whole resident set alive.

**D3. `scope: "all"` keeps today's behaviour.** It runs the streamed path:
batches of 8, release after each batch, the same ranking. It has no time
budget and no latency target (ruling 2). Its transient peak is about 8 × 90 MB plus open scratch.

**D4. Completeness contract.** Every recall_all response, on all three
surfaces, carries:

- `coverage`: `"complete"` or `"partial"` (an open string enum);
- `palaces_total`: palaces on disk at call start;
- `palaces_searched`: palaces actually searched, after open failures;
- `palaces_skipped`: provably empty palaces (meaning unchanged);
- `palaces_not_searched`: an integer, with `not_searched_by_reason`, a map
  from reason (`not_resident`, `open_failed`, `filter_failed`) to count;
- `open_failed`: the ids of palaces that failed to open.

The invariant is `palaces_total == palaces_searched + palaces_skipped +
palaces_not_searched`. `coverage` is `"complete"` only when
`palaces_not_searched` is 0. Zero palaces searched is a success with empty
results and `coverage: "partial"`, never an error. The chat tool and
`MemoryService::recall_all` move from a bare array to an object with
`results` plus these fields.

**D5. Timing.** D1-D4 ship in 0.x, before trusty-memory 1.0.0, because a
default-scope change after 1.0.0 changes what results mean (ADR-0066 D1.1).

## Acceptance criteria

1. A default-scope call with non-resident, non-empty palaces on disk makes
   zero `open_palace` calls; the registry id set is equal before and after.
2. A default-scope call does not reset a searched palace's idle clock; a
   test asserts `evict_idle` still evicts it on schedule.
3. A test with one palace that fails to open asserts it is counted under
   `open_failed` and not in `palaces_searched`. The D4 invariant holds on the
   MCP, service and chat surfaces.
4. `scope: "all"` returns the same top 5 as before the change for a fixed
   fixture. `recall_all_returns_open_palaces_to_baseline` still passes.
5. Live, on an installed build with 100+ palaces (60+ empty) and distinct
   queries: default-scope warm p95 under 1 s. The report states the host
   load average and the resident count.
6. The MCP tool description states the default scope and the meaning of
   `coverage`. The schema snapshot gains `scope`. `trusty-agents` passes the
   new fields through.

## Consequences

**Positive:**

- AC1 becomes reachable. A default call does R warm searches plus a header
  read per palace on disk (`palace.json`, and the #9141 empty check for
  non-resident palaces). It does no HNSW replay and no stranded scan. The
  header reads grow with palace count and belong in the AC5 measurement.
- A recall_all adds no RAM. Peak RAM follows the residency policy, which
  #6802 can size per machine.
- A partial answer says it is partial. This also fixes today's two silent
  cases: open failures and the inflated `palaces_searched`.
- `scope: "all"` keeps full coverage for callers that need it.

**Negative:**

- The default answer covers only the resident set. That reverses the #4637
  rule. A drawer in an idle palace is not found unless the caller asks for
  `scope: "all"`.
- The same query returns different results as residency changes. Tests must
  fix residency to be deterministic.
- A client that ignores `coverage` still fails open; the contract can only
  report.
- Useful default coverage depends on session pinning (#7087 slice 2, folded
  into #9141), which has not shipped. Until it ships, the resident set is
  whatever recent targeted calls left behind.
- `trusty-agents` cross-palace queries (`assistant_memory.rs:219`) get
  partial answers unless they opt in to `scope: "all"`.

## Related Decisions

Vetted against `docs/adr/INDEX.md` and PR #9176 on 2026-10-07:

- **ADR-0066 (trusty-memory 1.x contract):** Consistent. `scope` and the D4
  fields are additive; D5 lands the default change before 1.0.0.
- **ADR-0067 (Versioned palace format):** Consistent. No format change.
  Option 2 was rejected in part because it would need one.
- **ADR-0028 (Memory recall tiers):** Consistent. Tiers and demotion apply
  unchanged to whatever palaces are searched.
- **#4637 / #7125 rule in `helpers.rs:240-243`:** Conflict, resolved by this
  ADR (ruling 1). The rule becomes the `scope: "all"` behaviour.
- **ADR-0068 / 0069 / 0070 (memory sync, PR #9176):** Consistent. This ADR
  is numbered after them and merges after PR #9176.

## References

- [#9299](https://github.com/bobmatnyc/trusty-tools/issues/9299) recall_all must not cold-open palaces
- [#9141](https://github.com/bobmatnyc/trusty-tools/issues/9141) recall_all latency; part (a) [PR #9333](https://github.com/bobmatnyc/trusty-tools/pull/9333), 67c5535a84
- [#7087](https://github.com/bobmatnyc/trusty-tools/issues/7087) session-driven residency
- [#6802](https://github.com/bobmatnyc/trusty-tools/issues/6802) 24 GB support; [#6833](https://github.com/bobmatnyc/trusty-tools/issues/6833), [#6835](https://github.com/bobmatnyc/trusty-tools/issues/6835) HNSW sidecar and mmap
- [#4637](https://github.com/bobmatnyc/trusty-tools/issues/4637), [#7125](https://github.com/bobmatnyc/trusty-tools/issues/7125) prior open-every-palace rule
- [#9265](https://github.com/bobmatnyc/trusty-tools/pull/9265) stranded-point scan; [#9174](https://github.com/bobmatnyc/trusty-tools/issues/9174) vector-lane misses
- [PR #9176](https://github.com/bobmatnyc/trusty-tools/pull/9176) memory-sync ADRs
