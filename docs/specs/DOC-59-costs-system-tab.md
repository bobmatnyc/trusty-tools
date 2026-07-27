---
spec_refs:
  - id: SPEC-AGENTS-07~draft
    path: docs/specs/trusty-agents-product-spec.md
    anchor: SPEC-AGENTS-07~draft
  - id: SPEC-AGENTCFG-01~draft
    path: docs/specs/agent-config-five-sections.md
    anchor: SPEC-AGENTCFG-01~draft
  - id: SPEC-SHAREDWS-01~draft
    path: docs/specs/DOC-52-shared-workstream-definition.md
    anchor: SPEC-SHAREDWS-01~draft
---

# DOC-59 — Costs System Tab: Usage Telemetry, Attribution, and Cost Rollup

**Status:** Draft
**Subsystem:** trusty-agents — GUI costs view; usage capture and storage; cost calculation and attribution
**Owner:** Engineering (trusty-agents) / Bob Matsuoka
**Last-updated:** 2026-07-27
**Spec ID:** `SPEC-COST-01~draft` … `SPEC-COST-05~draft` (DOC-59)
**Epic:** #3959 (Costs system tab)
**Builds on:** DOC-54 [Trusty Agents Product Specification](./trusty-agents-product-spec.md) §7 (the GUI charter); DOC-57 [Five-Section Agent Configuration](./agent-config-five-sections.md) (agent identity and configuration); DOC-52 [Shared Workstream Definition](./DOC-52-shared-workstream-definition.md) (workstream semantics and attribution)
**Cross-ref:** `crates/trusty-agents/src/usage/` (usage logging and daily rollup); `crates/trusty-agents/src/llm/` (OpenRouter streaming path); `crates/trusty-agents/src/perf/` (pricing definitions); `crates/trusty-agents/src/agents/claude_code_runner/run.rs` (cache token parsing); `crates/trusty-agents/ui/src/views/` (GUI surfaces)

---

## 1. Executive Summary

The trusty-agents GUI currently displays a Chat tab and an Events tab. This spec introduces a third tab: **Costs**, showing daily, weekly, and monthly cost breakdowns by agent, model, and workstream. The feature addresses operator need for cost visibility and auditing across a multi-agent application.

Three owner decisions (established 2026-07-27) shape this spec:

1. **Workstream attribution uses `ws:<label>` memory-drawer tags**, joined to usage events via `session_id`. Classification runs post-hoc in a detached task, so the label is assigned after the response returns. This is the canonical workstream concept for billing — not the DOC-52 ledger Workstream type, which is product-level orchestration.
2. **Historical backfill is a non-goal.** The cost tracking clock starts at feature ship date. Old usage.jsonl rows lack session_id, cache tokens, and workstream metadata, making backfilled numbers unreliable. Explicit non-goal, not pursued.
3. **The Haiku-rate pricing bug** (a separate system hardcodes Sonnet roster at Haiku rates) is folded into this epic as a child issue (consolidating pricing to a single source of truth), not filed separately.

**Honest gap statement:** The GUI today has no usage-capture infrastructure in the default chat dispatch path. The streaming dispatcher (OpenRouter's `chat_stream`) parses provider usage blocks (including cache tokens) but does not emit them to any event. `ChatEvent` has no `Usage` variant. Without that foundational change, there is no per-model, cost-priced, workstream-attributed row to display — all downstream work (storage, rollup, API, GUI rendering) operates on an empty dataset.

This spec closes that gap and layers the cost features on top.

---

## 2. SPEC-COST-01 — Capturing and Recording Usage {#SPEC-COST-01~draft}

### 2.1 Definition (NORMATIVE)

**Usage** is a structured record of token consumption and wall-clock duration for a single LLM dispatch. A dispatch occurs whenever an agent calls an LLM (OpenRouter, Anthropic-direct, Bedrock, or claude-code CLI).

Each usage record MUST capture:

| Field | Type | Source | Required? |
|-------|------|--------|-----------|
| `session_id` | String (UUID4) | `TAGENT_RUN_ID` or `OPEN_MPM_RUN_ID` env var | Yes |
| `agent_name` | String | Agent's TOML-declared `[agent].name` | Yes |
| `model` | String | The model ID as dispatched (e.g. `anthropic/claude-sonnet-4-6`) | Yes |
| `runner_type` | String | One of `"openrouter"`, `"anthropic-direct"`, `"bedrock"`, `"claude-code"` | Yes |
| `prompt_tokens` | u32 | Provider's `input_tokens` or equivalent | Yes |
| `completion_tokens` | u32 | Provider's `output_tokens` or equivalent | Yes |
| `cache_read_tokens` | u32 | Provider's cache read tokens (e.g. OpenRouter's `cache_read_input_tokens`) | Yes |
| `cache_creation_tokens` | u32 | Provider's cache creation tokens (e.g. OpenRouter's `cache_creation_input_tokens`) | Yes |
| `duration_ms` | u64 | Wall-clock milliseconds for the LLM call | Yes |
| `ts` | String (RFC3339) | Timestamp when dispatch completed | Yes |
| `task_prefix` | String | First 60 characters of the task/prompt for human readability | No (advisory only) |

**Emission point (NORMATIVE):** A usage record is emitted immediately after the LLM provider's response is received and parsed — before any post-processing, guardrail checks, or output transformation. This ensures tokens are attributed to the original dispatch, not to subsequent work.

**Agent attribution (NORMATIVE):** `agent_name` MUST be threaded explicitly through the dispatch call chain (llm/stream.rs → OpenRouterProvider::chat_stream → record_dispatch_usage). Under multi-persona concurrency, reading the process-global `TAGENT_AGENT_ID` env var is incorrect — it may reflect a different agent if another dispatch ran concurrently. Env var reading is acceptable only in single-threaded contexts (e.g., the claude-code CLI runner, where process isolation holds).

**Cache tokens (NORMATIVE):** Cache tokens parsed from the provider response MUST be stored, not dropped. The OpenRouter streaming dispatcher already parses `cache_read_input_tokens` and `cache_creation_input_tokens` from the terminal usage block; adapters for other providers (Anthropic-direct, Bedrock) MUST do the same. Dropping parsed tokens defeats cost-tracking accuracy and hides infrastructure wins from the cache strategy.

---

## 3. SPEC-COST-02 — Storage and Rollup {#SPEC-COST-02~draft}

### 3.1 Definition (NORMATIVE)

**Storage Model:** Usage records are stored in an SQLite database (`.trusty-agents/state/usage.db`), replacing the unbounded append-only JSONL (`usage.jsonl`). SQLite provides:

- Atomic concurrent writes via journal locking
- Ad-hoc query capability without loading the entire log into memory
- Built-in datetime indexing for range queries
- Support for computed columns (e.g., derived `cost_usd`)

**Rollup Model:** Daily rolls aggregate records into rollup rows, keyed by `(date, agent, model, workstream)`, summing token counts and cost. Weekly and monthly rollups compose from daily rows. Rollup tables:

| Table | Rows keyed by | Aggregated fields |
|-------|---|---|
| `usage_daily` | `(date, agent, model, workstream)` | `prompt_tokens`, `completion_tokens`, `cache_read_tokens`, `cache_creation_tokens`, `duration_ms`, `cost_usd`, `count` (dispatch count) |
| `usage_weekly` | `(week_start, agent, model, workstream)` | Same as daily |
| `usage_monthly` | `(month_start, agent, model, workstream)` | Same as daily |

**Pricing (NORMATIVE):** Cost is computed at capture time using the pricing table in `crates/trusty-agents/src/perf/pricing.rs`. A snapshot of the rate applied to each record is stored in `usage_raw.cost_usd`. Price updates after a record is captured do not affect the recorded cost; this ensures historical auditability.

The existing hardcoded Haiku-only rate table at `usage/daily.rs:17-20` MUST be retired. All pricing sources converge on `perf/pricing.rs` (or its successor), which is checked into version control and documented via a manual price-update process tracked as a separate operational task.

---

## 4. SPEC-COST-03 — Workstream Attribution {#SPEC-COST-03~draft}

### 4.1 Definition (NORMATIVE)

A **workstream** is a durable label attached to a session for cost and work attribution. Workstreams are drawn from the `ws:<label>` memory-drawer tag convention defined in DOC-53 [Workstream Claim-Drawer Convention](./DOC-53-workstream-claim-drawer-convention.md) §2.

**Attribution binding:** A usage record's workstream is determined by joining its `session_id` to the memory store's tagged entries. The join is deferred — the workstream classification happens in a detached tokio::spawn after the response is returned to the caller, not on the critical path. If a session has multiple workstream tags (edge case: overlapping claims), the most recent tag wins; if none, the row is unattributed (workstream = NULL).

**Unattributed usage:** Rows with no workstream label still appear in cost reports, grouped under a reserved label `"(unattributed)"` in the GUI. This preserves visibility into all cost.

---

## 5. SPEC-COST-04 — HTTP API for Cost Queries {#SPEC-COST-04~draft}

### 5.1 Definition (NORMATIVE)

A new endpoint `GET /api/costs` exposes cost aggregates for the GUI.

**Request parameters:**

```
GET /api/costs?range=day|week|month&group_by=agent|model|workstream
```

| Parameter | Enum | Default | Meaning |
|-----------|------|---------|---------|
| `range` | `day`, `week`, `month` | `day` | Rollup granularity |
| `group_by` | `agent`, `model`, `workstream` | `agent` | Aggregation key |

**Response:** JSON array of cost rows:

```json
[
  {
    "date": "2026-07-27",
    "agent": "assistant",
    "model": "anthropic/claude-sonnet-4-6",
    "workstream": "feature-x",
    "prompt_tokens": 50000,
    "completion_tokens": 10000,
    "cache_read_tokens": 5000,
    "cache_creation_tokens": 1000,
    "cost_usd": 0.42,
    "dispatch_count": 3
  }
]
```

**Compatibility (NORMATIVE):** Rows spanning periods without attribution data are included with workstream = NULL. The GUI renders NULL workstreams as `"(unattributed)"`, following DOC-57's precedent of stating gaps rather than fabricating data.

---

## 6. SPEC-COST-05 — GUI Rendering {#SPEC-COST-05~draft}

### 6.1 Definition (NORMATIVE)

The Costs tab is a third tab in the main GUI view, alongside Chat and Events (DOC-54 §8.4).

**Tab title:** "Costs"

**Default display:** Daily costs grouped by agent. Three toggle controls below the chart:

1. **Granularity:** Day / Week / Month radio buttons
2. **Group by:** Agent / Model / Workstream dropdown
3. **Date range:** Start/end date pickers (optional; defaults to last 30 days)

**Rendering:**

- Primary visualization: stacked bar chart with dates on X-axis, USD cost on Y-axis, colored segments for each agent/model/workstream (depending on group_by).
- Below chart: sortable table with one row per grouping key, showing total cost, dispatch count, and token summary.
- Null workstreams displayed as `"(unattributed)"` in legend and table.
- No data for a date range: render text "No cost data for this period" rather than an empty chart.

**Architecture:** CostsView.svelte component matching existing Chat/Events surfaces. Depends on GET /api/costs endpoint (SPEC-COST-04).

---

## 7. Compatibility and Migration

**Backward compatibility (NORMATIVE):** Existing `usage.jsonl` files are never deleted; they remain as a read-only archive. New usage records go to SQLite. Migration of historical JSONL to SQLite is out of scope (non-goal per §1).

**Config:** No new agent configuration fields. Workstream attribution is purely a read from the memory store, not a declared agent property.

---

## 8. Phased Delivery

1. **Phase 1 — Foundational capture:** Add `Usage` variant to `ChatEvent`; thread cache tokens through dispatch; add `session_id` to usage records; emit from all dispatch paths (OpenRouter, Anthropic, Bedrock, claude-code CLI).
2. **Phase 2 — Storage and attribution:** Implement SQLite schema and daily rollup; join workstream tags via `session_id`.
3. **Phase 3 — Pricing and API:** Consolidate pricing; implement GET /api/costs.
4. **Phase 4 — GUI:** CostsView.svelte + tab wiring.

Load-bearing items (phases 1-2) must land before phase 3 and 4 are attempted; otherwise, cost reports will be empty or wrong.

---

## 9. Non-Goals

- **Historical backfill:** Old usage.jsonl rows are not migrated to SQLite or backfilled in the Costs tab. Feature starts at ship date.
- **Projected spend:** No forecasting or "at-this-rate-you'll-spend-$X-next-month" UI.
- **Cost allocation rules:** No complex attribution (e.g., "50% of this dispatch's cost goes to workstream-A, 50% to workstream-B"). Attribution is binary: a session is tagged with one workstream or none.
- **Pricing variance by region/tier:** Pricing is a flat table per model. No negotiated rate adjustments or tiered pricing.

---

## 10. Open Questions

None. All design questions were resolved in the owner sync on 2026-07-27 and are embedded as decisions in §1 and §2-5.

---

## 11. References

- DOC-52: Workstream Claim-Drawer Convention — defines `ws:<label>` tag semantics and memory-store binding.
- DOC-53: Five-Section Agent Configuration — defines agent identity and naming convention.
- DOC-54: Trusty Agents Product Specification — defines GUI surfaces and chat dispatch.
- perf/pricing.rs — canonical pricing table (source of truth for cost calculation).
- trust-agents usage module (`src/usage/`) — existing append-only JSONL logging.
- OpenRouter provider streaming path — parses and must propagate cache tokens.

---

## 12. Change Log

- **2026-07-27:** Initial draft. Owner decisions on workstream attribution (memory-drawer tags), backfill scope (none), and pricing consolidation integrated. Honest gap statement on missing `Usage` variant in `ChatEvent` added.
