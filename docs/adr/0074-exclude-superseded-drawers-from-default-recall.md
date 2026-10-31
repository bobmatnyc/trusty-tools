# 0074. Exclude superseded drawers from default recall

- **Status:** Accepted
- **Date:** 2026-10-31
- **Accepted:** 2026-10-31 (owner ruling D83, Bob, on [#10609](https://github.com/bobmatnyc/trusty-tools/issues/10609): "No, let's exclude them, except for historical searches.")
- **Scope:** crate `trusty-memory`: retire-on-write (`engine/retrieval/tier_c.rs`), the retirement marker (`engine/retired_marker.rs`, `tools/helpers.rs`), recall (`tools/recall_supersede*.rs`, `tools/recall_ops.rs`, `service/core_recall.rs`) and the `memory_recall`, `memory_recall_deep` and `memory_recall_all` tool schemas. Amends the read-side rule of ADR-0028 D6.
- **Reversibility Cost:** **Medium.** No row field, no palace-format bump and no data migration: the marker is one reserved tag. A rollback is "stop reading the tag and the exclusion". The expensive half is the contract: once agents rely on a replaced fact being absent, returning it again changes behaviour silently.
- **Decision Drivers:** a retired fact with stale instructions reached an agent through default recall (issue #10609, trusty-memory 0.36.2); ADR-0028 D6 relies on a score multiplier that fails open; owner ruling D83; the retirement that creates the marker is already one atomic write; ADR-0067 D3 makes a drawer-row field expensive.
- **Supersedes / Superseded by:** — (amends [ADR-0028](0028-memory-recall-tiers-standing-current-episodic.md) D6)

---

## Context

ADR-0028 D6 demotes a superseded fact. It loses Tier C privilege, stays readable, and gains a `superseded_by` pointer. Recall applied the demotion as a score multiplier (`SUPERSEDED_WEIGHT = 0.5`) found through a KG lookup with a 200 ms budget that fails open (`recall_supersede.rs`).

Issue #10609 records the failure. After a second `memory_note` to one `fact_key` slot, `memory_recall` returned the new drawer at 0.764 and the retired occupant at 0.295, with no marker. An agent resumed from the stale checkpoint.

Facts from the code that bound the options:

1. Retire-on-write clears the incumbent's `fact_key` and `expires_at` (`tier_c.rs`). Afterwards the retired drawer looks like any Tier E drawer, except through its `superseded_by` edge.
2. The edge is written after the retirement commit, by `link_retired`, and its failure is logged and ignored (#1713, #9433). A retiree can exist with no edge.
3. Reading the edge at recall time needs the KG, which has a time budget. A failed or late read finds no edge, which is indistinguishable from "nothing is superseded". Excluding on the edge alone therefore reproduces #10609 whenever the KG read fails.
4. Edges from other writers (the dream cycle, share, a hand-written `kg_assert`) carry no slot guarantee. ADR-0028 C9 found a hand-written edge pointing at the wrong drawer. `resolved_edges` already ignores an edge whose replacement is no drawer of the palace (#9462).
5. Drawer rows are positional postcard (`kg_redb/types.rs`). A new row field means a new `DrawerRecord` shape, and an older 1.x binary then cannot decode any rewritten row. ADR-0067 D3 treats that as a palace-format bump.

Options for the default rule:

- **A. Keep demotion.** Reproduces #10609. Rejected.
- **B. Exclude Tier C retirees only; demote and tag the rest.** Matches the guarantee in fact 2. Rejected by D83: "replaced facts" is the whole class.
- **C. Exclude every superseded drawer; a historical search opts back in.** Chosen (D83).

Options for the marker the exclusion reads:

- **i. The `superseded_by` edge only.** Fails open (fact 3). Rejected as the sole source.
- **ii. A new drawer row field.** Local and atomic, but hits fact 5: a format bump, a migration chain link and a fixture, and no safe downgrade. Rejected.
- **iii. A reserved tag on the retired drawer, written in the retirement commit.** Local, atomic, no format change, readable by an older binary as an ordinary tag. Chosen.

## Decision

We will amend ADR-0028 D6 as follows.

1. **Default recall excludes superseded drawers.** `memory_recall`, `memory_recall_deep` and `memory_recall_all`, and the in-process `MemoryService` recall methods (chat tools and chat context injection), drop every drawer known to be superseded. This runs before the score floor and the `top_k` cut, over the ranking window.

2. **"Superseded" has two sources.**
   - **Local marker.** The drawer carries the tag `superseded-by:<replacement uuid>`. Retire-on-write stamps it on the incumbent inside the same `upsert_drawers_atomic` commit that clears `fact_key` and `expires_at`, and on the in-memory drawer table under the same lock as the newcomer's push, because every recall lane and every later rewrite from the table (chunk split, dream merge) reads the table. Recall reads it off the drawer row. It needs no KG lookup, has no budget and cannot fail open.
   - **KG edge.** An active `superseded_by` edge whose replacement is a drawer of the palace, for supersessions no slot retirement made. Rulings palaces' edges count the same way.
   When both exist, the marker names the replacement.

3. **`include_superseded: true` is the historical search.** It is an optional boolean on the three MCP recall tools, default `false`, per call. It keeps superseded drawers, and each carries `superseded: true` and `superseded_by: "<replacement id>"`. Those drawers keep the D6 demotion, so they rank below their replacement (`SUPERSEDED_WEIGHT` and the below-the-replacement clamp apply only to them). The name mirrors the existing `include_creator_tags`, so the two opt-ins read alike, and says what the caller receives. `historical` was rejected: a boolean with that name does not say what it switches, and it reads like a time filter beside the temporal ranking. The in-process `MemoryService` methods have no opt-in.

4. **Failure behaviour.**
   - A slot retiree is excluded from its marker, so a KG error or a budget overrun cannot return it.
   - A supersession known only from the KG cannot be known when the read fails or runs past its 200 ms budget. The response then carries `supersession_unverified: true`, a WARN names the palace, and the recall still answers. Refusing the recall would turn a missing optional edge into an outage (#1713). The drawers that edge would have hidden may then appear in the default answer, and the flag says so.
   - The key is absent when the lookup answered, so a response with nothing to report keeps its prior shape. The in-process `MemoryService::recall_ranked` returns a list and has no field for the flag; it logs the WARN only.

5. **Dream merges do not copy the marker.** A dedup merge unions tags from the loser into the survivor. The union skips the `superseded-by:` namespace, so a live survivor is never hidden by a retiree it absorbed.

6. **Backfill is separate, report-first and human-gated.** It does not ship with this decision. Plan:
   1. A read-only report lists drawers that have an active `superseded_by` edge resolving to a drawer, and no marker, with the edge's creation time and the replacement id.
   2. A human reviews it.
   3. A write stamps the marker tag on the approved rows. It adds a tag and deletes nothing.
   Retirees written before the edge landed (#9433) have neither edge nor marker, are undetectable, and stay visible. They are not counted.

## Consequences

**Tier E readability is narrowed, not removed.** D6 point 2 says a superseded fact "remains a fully readable drawer in Tier E". Amended: it remains stored, readable by id and `memory_list`, and returned by recall only on `include_superseded`. ADR-0028's walkthrough said a session reaching a stale fact is led to the correct SHA by its `superseded_by` pointer. Amended: default recall does not reach the stale fact; a historical search does, with the pointer.

**ADR-0028's field budget still holds.** ADR-0028 Implementation scope records the D6 pointer as a KG triple and not a `Drawer` field, to stay inside its one-new-field accounting. The marker is a tag, so the accounting is unchanged.

**Positive.**
- The #10609 case cannot recur through default recall on any covered surface, whatever the KG does.
- Rollback needs no migration and an older binary reads the tag as ordinary data.

**Negative and costs.**
- A wrong edge now hides a drawer instead of halving its score (ADR-0028 C9). A cycle of edges (A to B to A) hides both members. `include_superseded` recovers them, and `resolved_edges` already drops an edge to no drawer. No cycle guard is added.
- An edge whose replacement drawer exists but cannot be read is dropped without setting `supersession_unverified` (`keep_resolved`, #9462).
- A replacement that is later forgotten, or expires, leaves the retiree marked and hidden by default; nothing clears the marker. The recovery is `include_superseded`, which returns the retiree with its `superseded_by` id. Edge cycles behave the same way.
- A window full of retirees returns fewer than `top_k`: the lanes fetch about `2 * top_k` (`ranking_window`) before `settle_superseded` removes the retirees, and no refill follows. A refill needs a re-fetch in each of the four recall paths (single palace, deep, all, rulings leg), so it was not added; a caller who hits the shortfall raises `top_k`. `fold_rulings` can also reserve a floor slot for a ruling that only a KG edge supersedes, when that lookup was unverified; the response is then flagged `supersession_unverified`. A ruling with a marker never takes a slot.
- The marker is a reserved tag namespace. Writes strip any caller-supplied `superseded-by:*` tag (`strip_retirement_tags`, applied in `parse_tags` for the MCP write tools and in the engine write pipeline for every other caller) and log a WARN; only `Drawer::mark_retired_by` adds it. Strip, not reject: an agent that re-saves the tags of a recalled historical row must not have its write fail, and the stripped tag was never the caller's to set. The tag is hidden from `tags` output (`project_tags`, so recall and `memory_list`) the way `creator:*` is; a historical search reports it as `superseded_by`. Importers: `import kuzu` generates prefixed tags only, and the share import keeps a record's tags, marker included, because it carries the replacement id with it.
- Callers that relied on seeing superseded drawers in default recall must pass `include_superseded`. Two existing tests did and now do.

**Surfaces.** Covered: `memory_recall`, `memory_recall_deep`, `memory_recall_all` (both scopes, both embedder states), the user-scope rulings leg, and `MemoryService::recall_ranked`, `recall` and `recall_all_scoped` (which serve the chat `recall_memories` tool, chat context injection and in-process callers). Not covered, with reasons:
- `get_prompt_context` and `prompt-context` serve KG hot triples (aliases, conventions, facts), never drawers, so no drawer supersession reaches them.
- `memory_list` is an enumeration, not a ranked recall.
- The health-probe recall in `transport/methods/health.rs` is a self-test.
- The `tm memory recall` verb in `trusty-mpm` calls `memory_recall`, so it gets the default exclusion; its flag for `include_superseded` is a follow-up in that crate.

**Scope of the implementation.** Marker stamp, the exclusion and its opt-in on the three tools, the tags, `supersession_unverified`, the merge guard. `live_while` remains out of scope as in ADR-0028.

## Related Decisions

Vetted against `docs/adr/INDEX.md` and prior decisions on 2026-10-31:

- **ADR-0028 (Memory recall tiers):** *Amends.* Changes the read-side rule of D6 (point 2) and the walkthrough's reachability claim. D3, D4, D5 and the one-new-field budget are unchanged.
- **ADR-0067 (Versioned palace format):** *Consistent.* A tag needs no format bump; a row field would have.
- **ADR-0066 (1.x compatibility contract):** *Consistent.* `include_superseded` is a new optional parameter, which D1.1 allows; the schema snapshot pins it.
- **ADR-0071 (Recall-all searches resident palaces by default):** *Extends.* `memory_recall_all` applies the same exclusion in either scope.
- **ADR-0027 (Rooms/Wings/Closets):** *Consistent.* No row is rewritten beyond the incumbent the retirement commit already rewrites.
