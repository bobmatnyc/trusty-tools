---
spec_refs:
  - id: SPEC-AGENTCFG-03~draft
    path: docs/specs/agent-config-five-sections.md
    anchor: SPEC-AGENTCFG-03~draft
---

# DOC-58 — Knowledge Section Addendum: K-d Attached Search Indexes

**Status:** Draft
**Subsystem:** trusty-agents — agent configuration model (Knowledge section, DOC-57 §4); `tools.search_indexes` config surface; trusty-search — index attachment
**Owner:** Engineering (trusty-agents) / Bob Matsuoka
**Last-updated:** 2026-07-26
**Spec ID:** `SPEC-KDIDX-01~draft` … `SPEC-KDIDX-06~draft` (DOC-58)
**Epic:** #4007 (Two-tier knowledge architecture: curated OKG stores vs arbitrary attached search indexes)
**Builds on:** DOC-57 [Five-Section Agent Configuration](./agent-config-five-sections.md) §4 `SPEC-AGENTCFG-03~draft` (the Knowledge section this addendum extends with a fourth sub-surface, K-d, alongside K-a/K-b/K-c); the backend ticket #3935 (`GET /api/agents/:name/knowledge`), whose scope this addendum amends rather than forks (§1)
**Cross-ref:** `crates/trusty-agents/src/agents/config.rs` (`ToolsConfig`, `.scopes` at :246-263 — the category-level `search.read` gate; `.search_indexes` to be added by #3232); `crates/trusty-agents/src/agents/extends/mod.rs` (`merge_extends`, `union_opt_vec` at :359, applied to `tools.allow`/`tools.scopes` at :290-292); `crates/trusty-agents/src/stores/config.rs` (`AgentStoreBinding` at :49-62, single-binding `validate()`); `crates/trusty-agents/src/tools/memory/vector_search.rs` (`effective_index_id` at :131-138, `daemon_query` at :265-288); `crates/trusty-search/src/service/server/indexes.rs` (`create_index`); `crates/trusty-search/src/mcp/tools/descriptors.rs` (`list_indexes`, `create_index` schemas at :195-208)
**Related issues:** #4007 (epic), #4008 (this ticket), #3232 (`tools.search_indexes` primitive — depended on), #3935 (Knowledge backend — amended, not reopened), #3890 (store-PATCH pattern, referenced for its read-only-until posture), #3936 (permissions backend, referenced not duplicated), #4009 (vector_search schema enrichment + optional allowlist enforcement), #4010 (Knowledge pane GUI: K-d sub-surface), #4011 (ops runbook + guardrail for `create_index` over an external/cross-org directory), #4012 (memory↔search M1), #4013/#4014 (memory↔search stretch tiers — out of scope here), #4015 (concrete driver: attach APEX to `cto-assistant`)

---

## 1. Why this is a new document, not an edit to DOC-57 or #3935

#4008 as filed proposes amending `docs/specs/agent-config-five-sections.md` directly (a new §4.1a). The owner's decision on how to land it (2026-07-26) is narrower than that: **K-d ships as its own addendum document that amends #3935's scope, and DOC-57 §4's text and #3935's already-written acceptance criteria are left untouched.**

Two reasons this is the safer shape, both stated by #4008 itself:

1. DOC-57 §4 (K-a/K-b/K-c) and #3935 (`C-03.1`…`C-03.4`) are load-bearing for work already building against them. Editing the source spec in place risks silently changing wording that other in-flight tickets cite by section number.
2. #4008's own scope statement is explicit: *"This ticket does not reopen that contract — it adds K-d as a fourth array in the same response shape, following #3935's own conventions."* An addendum file is the textual form of **adds**; an in-place edit would be the textual form of **reopens**.

**Consequence (NORMATIVE):** nothing in DOC-57 §4 or #3935 changes as a result of this document. Their K-1/K-2/K-3 posture and `C-03.1`–`C-03.4` conformance criteria continue to govern K-a/K-b/K-c unchanged; §5 below *restates* the same posture for K-d rather than editing §4's table to add a row.

---

## 2. SPEC-KDIDX-01 — The Two-Tier Principle {#SPEC-KDIDX-01~draft}

**ID:** SPEC-KDIDX-01~draft
**Status:** Draft

Quoted verbatim from epic #4007, the governing statement this whole addendum implements:

> **Two-tier principle:** OKG stores are the curated tier (tool-built via `okg_ingest_*`, exactly one per agent, `[[stores]]`). Arbitrary attached indexes are a second tier — user-added collections over any corpus, zero curation requirement, N per agent.

| | Tier 1 — OKG stores (K-a, DOC-57 §4.2) | Tier 2 — Attached indexes (K-d, this doc) |
|---|---|---|
| **Origin** | Built by the agent itself via `okg_ingest_*` tools | Attached by a user/operator to an index that already exists |
| **Curation** | Curated — the agent's own knowledge tree | Zero curation requirement — any corpus |
| **Cardinality** | Exactly one per agent (`StoresConfig::validate()` warns above 1) | N per agent |
| **Config surface** | `agent.toml` `[[stores]]` — `AgentStoreBinding{name,tree,index,palace}` | `agent.toml` `tools.search_indexes` (#3232) — `Vec<String>` of `index_id`s |
| **`extends` merge rule** | Child **REPLACES** (DOC-57 §2.3) | Child **UNIONS** (§3.2 below) |
| **Carries a palace?** | Optionally (`palace` field) | Never — no tree, no palace, no OKG concept at all |
| **`vector_search` role** | Supplies `default_index_id` (`vector_search.rs:96-99`) | Supplies an *additional* valid `index_id` the tool may target explicitly |

**Both tiers are read by the same query mechanism.** `VectorSearchTool::effective_index_id()` (`vector_search.rs:131-138`) does not distinguish where an `index_id` came from — a store-bound default and an attached K-d index are equally valid strings passed to `daemon_query()` (`vector_search.rs:265-288`), which issues `POST /indexes/{index_id}/search` against whichever it is given. K-d does not change that mechanism; it makes a second, arbitrary category of `index_id` **declared and discoverable** where today it is neither.

- **KD-1** OKG stores and attached indexes are never the same list. `tools.search_indexes` MUST NOT be populated from `[[stores]].index`, and `[[stores]]` MUST NOT be inferred from `tools.search_indexes`. Conflating them would silently widen the curated tier's single-binding ceiling by the back door — the exact lift OQ-1 (epic #4007) declines to make.

---

## 3. SPEC-KDIDX-02 — K-d: Attached Indexes Sub-surface {#SPEC-KDIDX-02~draft}

**ID:** SPEC-KDIDX-02~draft
**Status:** Draft

### 3.1 Definition (NORMATIVE)

**K-d joins K-a/K-b/K-c (DOC-57 §4.1) as a fourth Knowledge sub-surface**, answering a narrower question than the other three: *which arbitrary trusty-search indexes, beyond the agent's own curated store, may this agent's `vector_search` target?*

| Sub-surface | Source | Curated? | Cardinality |
|---|---|---|---|
| K-a — Store bindings | `[[stores]]` | Yes | 1 (DOC-57 §4.2) |
| K-b — Knowledge tools | Skills with `kind = "knowledge"` | N/A (tool set) | N |
| K-c — MCP knowledge connections | `[[tool_registry.endpoints]]` | N/A (transport) | N |
| **K-d — Attached indexes** | **`tools.search_indexes`** | **No** | **N** |

### 3.2 Config surface: `tools.search_indexes` (#3232)

```toml
[tools]
# Existing fields unchanged: allowed, allow, native, ast_native, scopes.
search_indexes = ["trusty-tools", "cto-projects"]
```

| Field | Type | Default | Merge rule across `extends` |
|---|---|---|---|
| `tools.search_indexes` | `Option<Vec<String>>` | `None` (no attached indexes) | **UNION**, base-first, deduped — the same `union_opt_vec` rule already applied to `tools.allow`/`tools.allowed`/`tools.scopes` (`extends/mod.rs:290-292`, `union_opt_vec` at `:359`) |

- **KD-2** Each entry is an `index_id` string that MUST already exist on the trusty-search daemon. `tools.search_indexes` is a **declaration of intent to use**, never a request to create — creation is `create_index` (`crates/trusty-search/src/service/server/indexes.rs`), a wholly separate action gated by #4011 (§6).
- **KD-3** Merge polarity is **UNION**, deliberately the opposite of K-a's REPLACE (DOC-57 §2.3). A base assistant's default project index and a child's added `cto-projects` index compose — a child agent gains attached indexes, it does not lose the base's. This mirrors `tools.allow`'s existing polarity (§9.3 of DOC-57: union is the safe default for an additive, local-config, never-committed file) and is the opposite of K-a's REPLACE, which exists specifically so a child does not inherit the base's default OKG target (DOC-57 §2.3).
- **KD-4** As of this writing (main @ `da75bc4e`), `tools.search_indexes` does not yet exist on `ToolsConfig` — #3232 is open, not merged. This addendum specifies the field's shape and semantics ahead of #3232 landing, per #4008's own dependency note ("should land first or in parallel"). Nothing in this document is blocked on #3232's merge order; the K-d response shape (§5) degrades to `attached_indexes: []` until the field is both declared in code and populated in an `agent.toml`.

### 3.3 Distinction from `[[stores]]`, restated structurally

A K-d entry carries **no** `tree`, **no** `palace`, and **no** curation requirement — just an `index_id`. It is not a degenerate or shorthand store binding; it is a different config key entirely, read by a different code path, with no cardinality ceiling. §2's table is the normative comparison; this subsection exists only to block the natural misreading that K-d is "`[[stores]]` without the tree field."

---

## 4. SPEC-KDIDX-03 — Enforcement Posture {#SPEC-KDIDX-03~draft}

**ID:** SPEC-KDIDX-03~draft
**Status:** Draft

### 4.1 Current state (no gating at all)

`VectorSearchTool::effective_index_id()` lets an explicit `index_id` tool-call argument override the bound default with **zero allowlist check** (`vector_search.rs:131-138`), and `daemon_query()` issues the search against whatever string it is given (`vector_search.rs:265-288`). `ToolsConfig.scopes` (`config.rs:246-263`) gates the search tool **family** via `search.read` — it has no concept of *which* `index_id` within that family is permitted.

### 4.2 M1 posture (NORMATIVE)

- **KD-5** At M1, `tools.search_indexes` is **declarative-only** — an opt-in allowlist that is *surfaced* (in the Knowledge pane, §6) but not yet *enforced*. This matches epic #4007's OQ-2 default assumption: declarative-only ships **no worse** than today's zero-gating state, because today has no allowlist concept whatsoever.
- **KD-6** Enforcement (a fail-closed check that an agent's `vector_search(index_id=…)` call is rejected when `index_id` is neither the store-bound default nor a member of `tools.search_indexes`) is **explicitly deferred to #4009** ("vector_search: schema enrichment + optional allowlist enforcement"). This document specifies the surface #4009 enforces against; it does not itself add a runtime check.

| Mechanism | Today | M1 (this addendum) | Future (#4009) |
|---|---|---|---|
| `search.read` scope | Gates the whole `vector_search`/search tool family | Unchanged | Unchanged |
| Per-index allowlist | Does not exist | Declared via `tools.search_indexes`, surfaced in the API/GUI, **not enforced** | Optionally enforced — fail-closed check against the declared list |
| `daemon_query` target validation | None — any string reaches `POST /indexes/{index_id}/search` | Unchanged | Optionally gated by #4009 |

---

## 5. SPEC-KDIDX-04 — Backend contract: extending #3935's route {#SPEC-KDIDX-04~draft}

**ID:** SPEC-KDIDX-04~draft
**Status:** Draft

### 5.1 Response shape addition

`GET /api/agents/:name/knowledge` (defined by #3935, unchanged in this addendum for `stores`/`tools`/`mcp`) gains a **fourth, additive array**:

```jsonc
{
  "stores":            [ /* K-a, StoreStatus — unchanged, #3935 */ ],
  "tools":              [ /* K-b — unchanged, #3935 */ ],
  "mcp":                [ /* K-c — unchanged, #3935 */ ],
  "attached_indexes": [
    { "id": "cto-projects", "label": "cto-projects", "connected": true,
      "chunk_count": 4213, "reason": null },
    { "id": "apex", "label": "apex", "connected": false,
      "chunk_count": null, "reason": "index not found on daemon" }
  ],
  "issues":            [],
  "config_error":       null
}
```

| Field | Type | Meaning |
|---|---|---|
| `id` | `string` | The `index_id` exactly as declared in `tools.search_indexes` |
| `label` | `string` | Display label; `id` verbatim in M1 (no separate display-name concept yet) |
| `connected` | `bool` | Whether the daemon currently reports this `index_id` as a live, queryable index |
| `chunk_count` | `number \| null` | From the daemon's index status when connected; `null` when not connected |
| `reason` | `string \| null` | Non-null exactly when `connected: false` — never a silent gap |

### 5.2 Posture, restated for K-d (not a new rule — #3935's own K-1/K-2/K-3 applied here)

- **KD-7 (= K-1 applied to K-d)** The route MUST NOT fail because `attached_indexes` resolution is degraded. Each of `stores`, `tools`, `mcp`, `attached_indexes` resolves independently; a daemon timeout while probing an attached index populates that entry's `reason` and leaves the other three sub-surfaces intact.
- **KD-8 (= K-2 applied to K-d)** No existing route changes shape or behavior. `GET /api/agents/:name/stores` and #3935's `stores`/`tools`/`mcp` fields are byte-identical to their pre-K-d definition.
- **KD-9 (= K-3 applied to K-d)** K-d ships **read-only** in M1 — `attached_indexes` is populated from `tools.search_indexes` in `agent.toml`; there is no `PATCH` in this document. Attach/detach as a write path is #4010's GUI concern layered on a future config-write route, exactly as K-3 defers store editing to #3890.

### 5.3 Backward compatibility (explicit, testable)

- **KD-10** An agent with no `tools.search_indexes` declared renders `"attached_indexes": []`, **byte-identical** to today's total absence of the K-d concept. No existing `/knowledge` consumer (were #3935 already shipped) observes any difference until an operator opts an agent in by adding `tools.search_indexes`.

---

## 6. SPEC-KDIDX-05 — GUI surface expectations for #4010 {#SPEC-KDIDX-05~draft}

**ID:** SPEC-KDIDX-05~draft
**Status:** Draft

- **KD-11** The Knowledge pane's K-d sub-surface supports **attach and detach of existing indexes only.** Attach appends an `index_id` to `tools.search_indexes` (a config write, a #3935/#3890-style `PATCH` follow-on — the write contract itself is out of scope for this addendum and lands with #4010); detach removes it. Neither action creates or deletes a trusty-search index.
- **KD-12** **Index *creation* stays CLI/ops-only, per #4011.** The GUI's attach picker lists only indexes the trusty-search daemon already reports via `list_indexes` (`crates/trusty-search/src/mcp/tools/descriptors.rs`) — it never offers an inline "create a new index" action. This is a deliberate narrowing: `create_index` over an external/cross-org directory (the APEX/Duetto driver, §8) writes a `.gitignore` inside a tree the GUI's operator does not own, which is exactly the confirmation-gated action #4011's runbook defines. A GUI shortcut around that gate would defeat the reason #4011 exists.
- **KD-13** Rendering follows DOC-57 §8.3's G-4 ("never fabricate"): an attached index that the daemon cannot currently reach renders as **NOT CONNECTED** with its `reason` (§5.1), never as silently absent and never as falsely connected — the same rule K-a's `StoreStatus` already follows.

---

## 7. SPEC-KDIDX-06 — Permissions interaction {#SPEC-KDIDX-06~draft}

**ID:** SPEC-KDIDX-06~draft
**Status:** Draft

- **KD-14** `search.read` (`ToolsConfig.scopes`, `config.rs:246-263`) remains the **only** permission gate on the search tool family, and it stays **category-level**: an agent either may or may not call `vector_search`/the search tool family at all. K-d introduces no new scope string and no new enforcement mechanism.
- **KD-15** **Index-level permission scoping — "agent X may query index A but not index B" — is explicitly out of scope for M1.** This is the same posture epic #4007's OQ-2 records ("declarative-only ships no worse than current state") and the same posture DOC-57 §7.2's PM-4 establishes for Permissions generally (no phase of the five-section model grants or narrows capability from a declarative surface alone). `tools.search_indexes` is a **knowledge-declaration** surface, not a **permissions-enforcement** surface, until #4009 optionally wires enforcement.
- **KD-16** Should index-level enforcement land (#4009), it is **additive to** `search.read`, not a replacement for it — an agent still needs `search.read` to reach the tool family at all; a `tools.search_indexes` allowlist would then further narrow *which* `index_id` that agent may target within the family. This spec does not commit #4009 to any particular mechanism (scope pattern, allow-list check, or otherwise) — only to the surface it would enforce against.

---

## 8. Worked example: APEX end to end (`cto-assistant` driver, #4015)

This walks the two-tier principle (§2) and K-d (§3–§7) through epic #4007's concrete driver, without prescribing #4015's implementation — it is illustrative, showing how the pieces defined above compose.

1. **Ops attaches the corpus (CLI, not GUI — KD-12).** An operator runs `create_index({id: "apex", root_path: "/Users/masa/trusty-mpm-projects/bob-duetto/apex"})` against the trusty-search daemon, following #4011's runbook (external/cross-org directory, explicit confirmation). This step touches trusty-search only; it does not touch `cto-assistant`'s config at all.
2. **`cto-assistant` declares the attachment (K-d, §3.2).** Its `agent.toml` gains:
   ```toml
   [tools]
   search_indexes = ["apex"]
   ```
   This is additive to, and independent of, its existing `[[stores]]` binding to the `cto` OKG tree and `cto` palace (K-a, untouched by this change — KD-1).
3. **The Knowledge pane reflects both tiers without conflating them (§5.1).** `GET /api/agents/cto-assistant/knowledge` now returns `"stores": [{"name": "cto-assistant", …}]` (K-a, unchanged shape) **and** `"attached_indexes": [{"id": "apex", "label": "apex", "connected": true, "chunk_count": N, "reason": null}]` (K-d, new).
4. **Querying needs no new code.** In chat, `vector_search(index_id="apex", query=…)` resolves via the existing, unmodified `effective_index_id()`/`daemon_query()` path (`vector_search.rs:131-138`, `:265-288`) — the mechanism to query an arbitrary `index_id` already works today with zero gating (§4.1). K-d's contribution is entirely that the attachment is now **declared** (in `tools.search_indexes`) and **discoverable** (in the Knowledge pane) — not that it enables a query path that did not exist.
5. **The curated tier is untouched.** `cto-assistant`'s own OKG store (`cto` tree, `cto` palace) is not widened, replaced, or affected by attaching APEX. The two tiers compose; they do not merge.

---

## 9. Non-Goals

1. **Index creation from the GUI.** Stays CLI/ops-only per #4011 (§6, KD-12).
2. **Index-level permission grants.** `search.read` stays category-level; per-index scoping is #4009's optional future concern, not this addendum's (§7, KD-15).
3. **The #4013/#4014 memory↔search stretch tiers** (search-hit-to-palace pin tool; palace-to-search-index ingestion adapter). Both are new cross-daemon plumbing epic #4007 explicitly separates from K-d's declare/discover surface.
4. **Lifting the `[[stores]]` single-binding ceiling.** K-d is a wholly separate tier-2 mechanism (epic #4007 OQ-1 default: keep `[[stores]]` single-binding as-is); this addendum does not touch `StoresConfig::validate()`.
5. **Enforcement itself.** M1 is declarative-only (§4.2); #4009 owns whether and how enforcement is added.
6. **Any change to #3935's existing K-a/K-b/K-c contract or acceptance criteria** — the explicit acceptance requirement of #4008 (§1).
7. **Editing DOC-57 §4's text in place** — the explicit form this addendum deliberately does not take (§1).
8. **A `tools.search_indexes` write/PATCH contract.** K-d's backend contract (§5) is read-only; the write path is #4010's concern, layered on whatever pattern #3890 establishes for store editing.

---

## 10. Conformance

- **C-KD.1** An agent with no `tools.search_indexes` renders `"attached_indexes": []` from `GET /api/agents/:name/knowledge`, identical to the pre-K-d response shape (KD-10).
- **C-KD.2** An `index_id` in `tools.search_indexes` that does not exist on the trusty-search daemon renders `connected: false` with a non-null `reason` — never omitted, never silently dropped (KD-7, mirroring `C-03.1`).
- **C-KD.3** A daemon timeout while probing one attached index leaves `stores`, `tools`, and `mcp` unaffected in the same response (KD-7).
- **C-KD.4** `tools.search_indexes` across `extends` resolves by UNION, base-first, deduped — never REPLACE (KD-3). A regression test asserts this against `merge_extends` once #3232 lands.
- **C-KD.5** No `PATCH` route or GUI action in the current phase creates or deletes a trusty-search index; the attach-picker's candidate list is sourced from `list_indexes`, never from a free-text "new index" field (KD-12).
- **C-KD.6** An agent lacking `search.read` cannot invoke `vector_search` regardless of what `tools.search_indexes` declares — K-d adds no bypass of the existing category-level gate (KD-14).
- **C-KD.7** `GET /api/agents/:name/stores` and #3935's `stores`/`tools`/`mcp` fields are unchanged in shape and content by this addendum (KD-8).

---

## 11. References

**Extends (without editing):**
- DOC-57 [Five-Section Agent Configuration](./agent-config-five-sections.md) §4 `SPEC-AGENTCFG-03~draft` — the Knowledge section's K-a/K-b/K-c, whose text and conformance criteria are unchanged by this document.
- #3935 — the `GET /api/agents/:name/knowledge` backend ticket; this addendum amends its scope with a fourth array (§5), per #4008's explicit "amend not fork" instruction.

**Depended on:**
- #3232 — `tools.search_indexes: Option<Vec<String>>` + union-extends merge, the core config primitive this addendum specifies the semantics of (§3.2, KD-4).

**Referenced, not duplicated:**
- #3890 — store-PATCH contract; K-d's read-only-until-PATCH posture (KD-9) mirrors its pattern rather than reopening it.
- #3936 — permissions backend; index-level enforcement, if ever built, lives under its scopes model (§7, KD-16), not a parallel one invented here.

**Sibling children of epic #4007 (context, not specified here):**
- #4009 — vector_search schema enrichment + optional allowlist enforcement (§4.2).
- #4010 — Knowledge pane GUI: K-d sub-surface, attach/detach (§6).
- #4011 — ops runbook + guardrail for `create_index` over an external/cross-org directory (§6, §8).
- #4012 — memory↔search M1 (persona-memory context fold-in) — orthogonal to K-d.
- #4013 / #4014 — memory↔search stretch tiers — explicit non-goals (§9.3).
- #4015 — concrete driver: attach APEX to `cto-assistant` end to end (§8).

**Related issues:**
- #4007 — epic: Two-tier knowledge architecture.
- #4008 — this ticket.

---

## 12. Change Log

- **2026-07-26** — Initial addendum (DOC-58, `SPEC-KDIDX-01~draft` … `-06~draft`). Introduces K-d (attached search indexes) as a fourth Knowledge sub-surface alongside DOC-57 §4's K-a/K-b/K-c, backed by `tools.search_indexes` (#3232) with union-extends merge semantics. Specifies the `attached_indexes[]` addition to #3935's `/knowledge` route (declarative, degrade-never-fail, read-only), an M1 enforcement posture of opt-in-allowlist-declared-but-not-enforced, GUI expectations for #4010 (attach/detach existing indexes only, creation stays CLI-only per #4011), and the permissions interaction with `search.read` (category-level, unchanged). Records the APEX/`cto-assistant` driver (#4015) as a worked example and lists explicit non-goals, including the #4013/#4014 stretch tiers. Amends #3935's scope per #4008's acceptance criteria; does not edit DOC-57's or #3935's existing text.
