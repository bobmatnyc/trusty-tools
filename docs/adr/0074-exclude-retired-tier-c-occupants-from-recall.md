# 0074. Exclude retired Tier C slot occupants from default recall

- **Status:** Proposed
- **Pending:** owner ruling D83 (#10609)
- **Date:** 2026-10-31
- **Scope:** crate `trusty-memory` (retire-on-write in `engine/retrieval/tier_c.rs`, recall in `tools/recall_supersede.rs` and the recall tools, prompt-context injection). Amends the read-side behaviour of ADR-0028 D6.
- **Reversibility Cost:** **Medium.** Adds one drawer field (`retired_at`). The field is additive, and a rollback is "stop reading it". The exclusion rule is the expensive half to reverse once agents depend on it.
- **Decision Drivers:** a retired Tier C slot occupant with stale instructions reached an agent through default recall (issue #10609, trusty-memory 0.36.2); the demotion that ADR-0028 D6 relies on is a score multiplier that fails open; the retirement that creates the marker is already atomic.
- **Supersedes / Superseded by:** Amends ADR-0028 (D6, the Consequences walkthrough, and the Implementation scope's D6 storage note).

---

## Context

ADR-0028 D6 demotes a superseded fact: it loses Tier C privilege, stays readable, and gains a `superseded_by` pointer. Recall applies the demotion as a score multiplier (`SUPERSEDED_WEIGHT = 0.5`, `tools/recall_supersede.rs:41`). The demotion needs a KG lookup, which has a 200 ms budget (`LOOKUP_BUDGET`, `recall_supersede.rs:49`) and fails open (`fail_open`, `recall_supersede.rs:86`).

Issue #10609 records the failure. After a second `memory_note` to the same `fact_key` slot, `memory_recall` returned the new drawer at 0.764 and the retired occupant at 0.295. The retired occupant had no marker. An agent resumed from its stale checkpoint.

Three facts from the code bound the options:

1. Retire-on-write clears the incumbent's `fact_key` and `expires_at` (`tier_c.rs:396-404`). After retirement, a retired drawer is indistinguishable from an ordinary Tier E drawer, except through its `superseded_by` edge.
2. The edge is written by the same atomic slot move that retires the incumbent (`link_retired`, `tier_c.rs:428`). For Tier C, the edge is not inferred. It links the actual incumbent to the actual replacement.
3. ADR-0028 C9 showed that edges written by hand, or inferred, can point at the wrong drawer. Edges from other writers (manual `supersedes-<id>` tags under D5, dreamer and consolidation writers) do not carry guarantee 2.

Options for the default-recall rule:

- **A. Keep demotion.** Reproduces #10609. Rejected.
- **B. Exclude every superseded drawer.** Simple. It hides any drawer whose edge is wrong, which C9 shows happens. It also overturns D6's "demotion, not exclusion" for drawers that were never retired from a slot.
- **C. Exclude Tier C retirees only; demote and tag all other superseded drawers.** Matches the guarantee in fact 2. Other superseded drawers keep D6 unchanged.

Options for the marker that the exclusion reads:

- **i. Read the `superseded_by` edge at recall time.** Needs the KG, so it depends on the 200 ms budget and fails open. Rejected for the exclusion rule. It remains right for the tag on non-Tier-C drawers.
- **ii. Add a drawer field, `retired_at`, written in the same `upsert_drawers_atomic` commit as the retirement.** Local, atomic, no KG read. Costs a second field, which exceeds the one field ADR-0028's Reversibility Cost budgets.
- **iii. A side index keyed by drawer id.** Avoids a field on the drawer. Adds an index, which is also outside ADR-0028's budget, and needs a second write path kept in step with the retirement.

## Decision

We will amend ADR-0028 D6 as follows.

1. **Retirement stamps a marker.** Retire-on-write sets `retired_at` on the incumbent in the same commit that clears `fact_key` and `expires_at`. The marker is a drawer field, and the retirement stays one atomic write.

2. **Default recall excludes marked drawers.** `memory_recall`, `memory_recall_all`, `memory_recall_deep`, and prompt-context injection drop every drawer with `retired_at` set. The check is a field comparison, applied before top-k truncation. It reads no KG, so it has no budget and no fallback path. Every recall surface shares this one filter.

3. **Exclusion is limited to Tier C retirees.** A drawer superseded through any other `superseded_by` edge is not excluded. It keeps the D6 demotion.

4. **Every superseded result is tagged.** Any recalled drawer with a `superseded_by` edge carries `superseded: true` and `superseded_by: <id>`. This applies to demoted drawers too, so a client can ignore a demoted drawer that still ranks.

5. **`include_superseded: true`** returns the excluded Tier C retirees, carrying the same tags. The default is `false`. The parameter is per call. It is not a palace setting.

6. **Failure behaviour.**
   - The Tier C exclusion fails closed by construction. It reads a field on the drawer row. A read error already fails the recall, so no retiree is returned on error.
   - The tag for non-Tier-C superseded drawers fails open, as D6 already does. When the KG lookup errors or exceeds its budget, the response sets `supersession_unverified: true`. The drawer is returned untagged, never silently.
   - No KG error changes what is excluded.

## Consequences

**Tier E readability is narrowed, not removed.** D6 says a superseded fact "remains a fully readable drawer in Tier E." Amended: a Tier C retiree remains stored, and is readable by id and through `include_superseded`. It is not in default ranked recall. Other superseded drawers are unchanged.

**Walkthrough (#4818 / #4820).** The Proposed outcome said a session that reaches the stale fact is "led to the correct SHA" by its `superseded_by` pointer. Amended: default recall does not reach the stale fact at all. It is reached only by `include_superseded` or id lookup, and then carries the pointer.

**Positive.**
- The #10609 case cannot recur through default recall, on any recall surface.
- The exclusion is local and atomic with the retirement that creates it. It adds no KG round trip to the recall path.

**Negative and costs.**
- A second drawer field. ADR-0028's Reversibility Cost accounting becomes two new fields (`fact_key`, `retired_at`) and one new index. This amendment is the record of that change.
- The no-rewrite invariant in ADR-0028 Migration is already qualified by retire-on-write, which clears `fact_key` on the incumbent. The marker adds one field in that same write. It does not extend the rewrite set.
- Retirees written before the #9433 edge landed have no `superseded_by` edge and no marker. They stay visible as ordinary drawers. The residual count is not measured.

**Migration (backfill).** Retirees that have a `superseded_by` edge may be backfilled. Steps:
1. A read-only report lists retirees with an edge, with the edge's creation time and the replacement id.
2. A human reviews the report.
3. A write sets `retired_at` from the edge's creation time. It adds one field and deletes nothing.
Retirees with no edge are listed in the report and left untouched.

**Rollback.** Stop reading `retired_at`. The default returns to D6 demotion. No data is lost.

**Scope of the implementation wave.** Retirement marker, the field filter across the four recall surfaces, the `superseded` and `superseded_by` tags, `include_superseded`, and the backfill report. `live_while` remains out of scope, as in ADR-0028 Implementation scope.

## Related Decisions

Vetted against `docs/adr/INDEX.md` and prior decisions on 2026-10-31:

- **ADR-0028 (Memory recall tiers — Standing, Current, Episodic):** *Amends.* Changes the read-side rule of D6 for Tier C retirees, and the walkthrough's reachability claim. D3, D4 and D5 are unchanged. The D6 storage note in Implementation scope gains a second field.
- **ADR-0071 (Recall-all searches resident palaces by default):** *Extends.* `memory_recall_all` applies the same retirement filter in every resident palace it searches.
- **ADR-0027 (Rooms/Wings/Closets):** *Consistent.* The change adds a field and does not rewrite or delete a drawer row beyond the marker written in the retirement commit.

---

## Open questions for Bob (D83)

1. **Exclusion scope.** Option C (Tier C retirees only) is recommended. Confirm, or rule for option B.
2. **Marker.** Option ii (`retired_at` on the drawer) is recommended. It exceeds the ADR-0028 field budget. Confirm the budget change, or choose the side index (iii).
3. **Interim fix.** Should the tag-only change (point 4, no exclusion) ship first as the P1 hotfix, then the exclusion once `retired_at` lands? The tag alone does not stop an agent reading a stale retiree in a ranked list.
4. **Pre-#9433 retirees.** These have no edge and no marker. Accept the residual, or require a separate detection pass?
5. **Surfaces.** Confirm `memory_recall_deep` and prompt-context injection take the same filter as `memory_recall`. The prompt-context path (`prompt-context-facts`) was not traced in this draft.
6. **Override.** Should `include_superseded` be available to prompt-context injection, or only to explicit recall calls? The draft says explicit calls only.
7. **Backfill approval.** Is the edge-based backfill (migration step 3) approved as a human-gated write, or must it remain report-only?
