# Structured Dream Consolidation with Tool-Calling and Tombstone Archival

Status: ACCEPTED — §9 decisions resolved by Bob 2026-07-16; PoC implemented under epic #2866
Scope: `trusty-memory` (crate) + `trusty-common::memory_core::dream` / `semantic_consolidation` / `dream_consolidation`
Author: research pass, grounded in `crates/trusty-tools` worktree `tm-trusty-tools-01` @ `bbe3f0c8`

> **Citation-drift note (2026-07-16):** file:line citations below were taken
> against `bbe3f0c8`; `origin/main` has since moved (notably the
> fading-memories pass #2352 has landed in `dream/`). Where a citation and
> current code disagree, current code wins. Known drift is annotated inline
> with **[drift]** markers; unannotated line numbers may be off by a few
> lines but the named symbols remain authoritative.

This is **not** a greenfield feature. A semantic-consolidation LLM pass already
runs inside the dream cycle today. This spec upgrades it: replace free-text
response parsing with structured tool-calling, and close a pre-existing
correctness gap where consolidation silently grows the palace instead of
compacting it.

---

## 1. Motivation / problem

A memory palace only grows. Every `memory_remember` adds a drawer; nothing
today removes or compresses the accumulated history except:

- the strict NLP dedup pass (near-identical content only, `dedup_threshold`
  default `0.95` — `crates/trusty-common/src/memory_core/dream/config.rs:68`),
- a content-quality prune (blocklist / too-short — `content_prune_pass`,
  `crates/trusty-common/src/memory_core/dream/cycle.rs:40-72`),
- an existing but incomplete LLM "semantic consolidation" phase (see §2.3)
  that adds canonical summaries but **never removes or hides the originals**.

Ontological/ER facts (subject–predicate–object triples) are otherwise only
created by hand via `kg_assert`, or heuristically by the `kg_extract`
"is-a" / tag / hashtag pattern-matcher on every `memory_remember`
(`crates/trusty-memory/src/kg_extract.rs`, wired at
`crates/trusty-memory/src/tools/helpers.rs:312-333`). There is no path today
where an LLM reads a *cluster* of related memories and derives new facts from
them — only per-drawer heuristics.

Net effect: recall precision degrades as the palace fills with near-duplicate
and superseded drawers, and the knowledge graph stays shallow because nobody
is deriving relationships between memories, only extracting them from single
drawers in isolation.

This spec closes both gaps in one pass: a cluster of memories goes to a cheap
tool-calling model, which returns a summary, freeform inferences, and
structured ER facts; the summary + facts are stored as new memories / KG
assertions, and the **source memories are tombstoned (not deleted)** with a
provenance back-link.

---

## 2. Current state

### 2.1 Dream cycle — what exists and where

The dream cycle lives in `crates/trusty-common/src/memory_core/dream/`:

| File | Role |
|---|---|
| `dreamer.rs` | `Dreamer` struct; `dream_cycle()` orchestrates every pass in order (content-prune → dedup → prune → compact → closet refresh → semantic consolidation) — `dreamer.rs:36-59` for the struct, cycle body from `dreamer.rs:248` onward |
| `cycle.rs` | The passes themselves: `content_prune_pass` (`cycle.rs:40`), `semantic_consolidation_pass` (`cycle.rs:515`), `apply_consolidation_result` (`cycle.rs:366`), `record_provenance_and_collect_superseded` (`cycle.rs:427`), `refresh_closets`/`build_closet_index` |
| `config.rs` | `DreamConfig` (defaults: `idle_secs: 300`, `dedup_threshold: 0.95`, `max_cycle_ms: 60_000` — `config.rs:66-82`), `DreamStats`, `PersistedDreamStats` (written to `<palace>/dream_stats.json`) |
| `guard.rs`, `helpers.rs`, `recall_benchmark.rs` | compaction guard, shared helpers, before/after recall-quality benchmark |

MCP surface: `palace_dream` and `dream_consolidate_room` both dispatch to
`handle_dream_consolidate_room` (`crates/trusty-memory/src/tools/dream_ops.rs:38-77`,
alias at `:91-93`), which builds a `DreamConfig` from the daemon's user config
and calls `consolidate_scoped` — the on-demand, room-scoped sibling of the
same pipeline the idle loop runs.

**Correction to a stale prior belief:** the autonomous scheduler is **not
dormant**. `crates/trusty-memory/src/dream_scheduler.rs:1-21` documents that
issue #1529 wired `Dreamer::start_with_shutdown()` into daemon startup — it
was implemented but never called before that fix; it is called now.
`crates/trusty-memory/src/main.rs:895-899` calls
`trusty_memory::dream_scheduler::spawn_dream_scheduler(&bg_state.registry,
dream_shutdown_rx)` unconditionally on daemon startup (after palace
hydration), spawning one background `Dreamer` loop per palace that fires
every `idle_secs` (default 300s) once idle. The only way to disable it is the
`TRUSTY_DREAM_DISABLED` env var (`dream_scheduler.rs:39`, checked at
`dream_scheduler.rs:69`). So: **the scheduler runs by default today; whether
the semantic/LLM phase inside each cycle actually does anything depends on
whether an inference backend is configured** (§2.3 — it fails open/no-ops
when not).

**Note on issue #2352 (fading-memories resurface pass) — [drift: RESOLVED]:**
the paragraph below described the `bbe3f0c8` state; as of current
`origin/main` the fading pass HAS landed: `dream/fading.rs` exists,
`DreamConfig` carries a `fading: FadingParams` field, `DreamStats` carries a
`fading: Vec<FadingMemory>` list, and `dream_ops.rs` surfaces the list in the
MCP response. The fading pass is detection-only (it never boosts or recalls),
so it does not consume the §4.3 recall filter; threading the archived-filter
through `detect_fading`'s candidate selection remains explicitly deferred
fast-follow work (§7), not part of the PoC.

### 2.2 What "clustering" means today — less than the config implies

`SemanticConsolidationConfig` (`crates/trusty-common/src/memory_core/semantic_consolidation/types.rs:29-56`)
has a `similarity_threshold: f32` field (doc comment: "candidates for
LLM-based consolidation") and `max_batch_size: usize`. The doc comment on
`SemanticConsolidator` itself even claims it operates on drawers "already
filtered to candidates via the embedding-similarity threshold"
(`consolidator.rs:36-39`).

**This is not true of the actual implementation.** `SemanticConsolidator::consolidate`
(`crates/trusty-common/src/memory_core/semantic_consolidation/consolidator.rs:71-120`)
does exactly one thing: `drawers.chunks(self.config.max_batch_size)` — naive,
order-preserving, fixed-size batches over whatever slice it's handed. No
embedding similarity is computed anywhere in this module.
`similarity_threshold` is **read nowhere** in `consolidator.rs`. Today a
"cluster" is just "the next 8 drawers in snapshot order from the palace" (or
from one room, for the on-demand tool). This is an existing gap, not
something this spec introduces — flagged here because it directly bears on
§3.2's cluster-selection design and §9's open decision.

### 2.3 Inference provider — TWO separate abstractions exist; only one does tool-calling

**(a) The existing free-text `Inference` trait used by the current semantic
pass — chat-only, no tool-calling.**
`crates/trusty-common/src/memory_core/semantic_consolidation/inference.rs`:
`trait Inference { async fn consolidate(&self, drawers: &[Drawer]) -> Result<Vec<ConsolidationAction>>; }`
(`inference.rs:52-65`). `OpenRouterInference` (`inference.rs:77-160`) and
`OllamaInference` (`inference.rs:170-248`) both POST a plain, non-streaming
`/v1/chat/completions` body (`"stream": false`, no `tools` field at all) and
then hand the raw text `content` to `parse_consolidation_actions`
(`semantic_consolidation/types.rs:195-223`), which strips markdown fences,
finds the first `[...]` substring, and best-effort `serde_json::from_str`s
it — falling back to an **empty** action list on any parse failure
(`types.rs:212-222`). This is exactly the "fragile free-text parsing" pattern
Bob flagged. It has no notion of tool/function calling whatsoever.

**(b) The pluggable, tool-calling-capable provider Bob asked us to find — it
already exists, is already a `trusty-memory` dependency, and is already
instantiated in `trusty-memory` today.**

Trait: `trait ChatProvider: Send + Sync` —
`crates/trusty-common/src/chat/mod.rs:160-172`:
```rust
async fn chat_stream(
    &self,
    messages: Vec<ChatMessage>,
    tools: Vec<ToolDef>,
    tx: Sender<ChatEvent>,
) -> Result<()>;
```
Supporting types, same file: `ToolDef { name, description, parameters:
serde_json::Value }` (`mod.rs:103-107`), `ToolCall { id, name, arguments:
String }` (`mod.rs:121-125`), `ChatEvent::{Delta, ToolCall, Done, Error}`
(`mod.rs:139-144`). This **is** OpenAI-style function calling: pass a
non-empty `tools` vec, and the provider streams back `ChatEvent::ToolCall`
events as the model invokes them (verified live by
`crates/trusty-common/src/chat/openai_compat/mod.rs:194-259`,
`ollama_provider_emits_tool_call`, which round-trips a real streamed
`tool_calls` SSE payload into a parsed `ToolCall{id, name, arguments}`).

Concrete implementations, all in `crates/trusty-common/src/chat/`:
`OpenRouterProvider` and `OllamaProvider` (`openai_compat/providers.rs`,
both implement `chat_stream` by POSTing OpenAI-compatible
`/v1/chat/completions` with `tools_wire(&tools)` — `openai_compat/wire.rs:50-68`),
and `BedrockProvider` (feature-gated: `bedrock_impl.rs` under the `bedrock`
feature, `bedrock_stub.rs` fallback that errors clearly when the feature
isn't compiled in).

**`trusty-memory` already depends on this and already uses it** — this is
not a new coupling. `AppState::chat_provider()` —
`crates/trusty-memory/src/lib.rs:1260-1281` — builds an
`Arc<dyn ChatProvider>` from the daemon's user config (local model first via
`trusty_common::auto_detect_local_provider`, else
`trusty_common::OpenRouterProvider::new(cfg.openrouter_api_key,
cfg.openrouter_model)`), and it is **actively called today**, not dead code:
`crates/trusty-memory/src/chat/handler.rs:35`, `:112`, and
`crates/trusty-memory/src/chat/sessions.rs:44` all call
`state.chat_provider().await` for `trusty-memory`'s own chat-session feature.

Dependency graph / cycle risk: `crates/trusty-memory/Cargo.toml:83` already
declares `trusty-common = { path = "../trusty-common", ... }`.
`crates/trusty-common/Cargo.toml` has **zero** `trusty-*` path dependencies
(confirmed by grep — it is the workspace's foundational leaf crate). There is
no cycle: `trusty-common` cannot depend back on `trusty-memory`, and
`trusty-memory` already depends on `trusty-common`. Reusing `ChatProvider`
for the new consolidation pass adds no new edges to the dependency graph.

**Configuration** (existing, unchanged by this spec): `~/.trusty-memory/config.toml`,
sections `[openrouter]` (`api_key`, `model`) and `[local_model]` (`enabled`,
`base_url`, `model`) — parsed by `UserConfigMin`/`LocalModelMin`
(`crates/trusty-memory/src/service/helpers.rs:309-354`) via
`load_user_config()` (`helpers.rs:382-404`). Defaults:
`openrouter_model` = `"anthropic/claude-3-5-sonnet"` when unset
(`helpers.rs:390-394`); `LocalModelConfig::default()` = Ollama at
`http://localhost:11434`, model `qwen3:30b` (`chat/mod.rs:80-88`). Note this
loader does **not** layer an `OPENROUTER_API_KEY` env var over the toml file
(unlike `trusty-search`'s equivalent loader, which does — `trusty-search/src/service/config.rs:113-156`);
the *existing* `semantic_consolidation` phase's own `build_consolidator_from_config`
(`crates/trusty-common/src/memory_core/dream/cycle.rs:323-349`) has its own,
separate env-var fallback via `crate::env_vars::ENV_OPENROUTER_API_KEY` when
`config.openrouter_api_key` is empty. Two independent env-var-fallback code
paths exist in two different places; this spec's new pass should reuse the
`ChatProvider`-based path (`AppState::chat_provider()`), which reads only the
`.trusty-memory/config.toml` today — see §5 for the config surface this
spec adds.

**(c) `trusty-code`'s independent provider stack** (`crates/trusty-code/src/provider/{openrouter,bedrock,fireworks,together,atlascloud}.rs`)
also has full tool-calling (`ToolChoice`, `map_tool_choice`) but is scoped to
`trusty-code`'s own agent-loop runtime, is not a `trusty-memory` dependency,
and per Bob's decision is **not** the provider this work uses — noted only
for completeness of the "does tool-calling exist anywhere" question.

### 2.4 KG / facts storage

Triples live in a redb-backed `KnowledgeGraph`
(`crates/trusty-common/src/memory_core/store/kg/graph.rs:33-54`, migrated off
SQLite in issue #989 — `migrate_from_sqlite_if_needed` at `graph.rs:78-80` is
now a documented no-op). Schema
(`crates/trusty-common/src/memory_core/store/kg/types.rs:44-56`):

```rust
pub struct Triple {
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub valid_from: DateTime<Utc>,
    pub valid_to: Option<DateTime<Utc>>,   // bi-temporal: re-asserting closes the prior interval
    pub confidence: f32,                    // [0.0, 1.0]
    pub provenance: Option<String>,         // free-form: drawer id, source, agent name, ...
}
```

Key methods: `assert(triple)` (`kg/graph.rs:149`) — closing any prior
interval for the same subject+predicate; `retract(subject, predicate)`
(`kg/graph.rs:181`); `query_active(subject)` (`kg/ops.rs:29`); graph
traversal (`neighbors`, `incoming`, `shortest_path`, `reachable`) per the
module doc at `kg/graph.rs:1-9` — **`incoming` already exists**, which
matters for §4 (no need for a second, reverse triple to answer "what sources
back this summary").

MCP tools `kg_assert` / `kg_query` dispatch through
`MemoryService::kg_assert` (`crates/trusty-memory/src/service/core_kg.rs:38-54`)
straight onto `Triple`/`handle.kg.assert`. `kg_bootstrap` is unrelated — it
scans project files (git remotes, `go.mod`, etc.) for structural facts at
project-init time (`crates/trusty-memory/src/bootstrap/`), not memory
content.

Auto-extraction already exists, but it is a **per-drawer heuristic, not an
LLM, and not cluster-aware**: `extract_triples` (`crates/trusty-memory/src/kg_extract.rs`)
pattern-matches "X is a Y" → `is-a`, tags → `tags`, hashtags → `mentioned-in`,
room → `contains`, wired automatically on every remember via
`auto_extract_and_assert` (`crates/trusty-memory/src/tools/helpers.rs:312-333`).
This spec's new pass is the first place an LLM derives triples from a
*group* of related memories rather than one drawer read in isolation.

### 2.5 Archival — no tombstone exists today; only hard delete

`memory_forget` → `MemoryService::delete_drawer`
(`crates/trusty-memory/src/service/core.rs:569-591`) → `PalaceHandle::forget`
(`crates/trusty-common/src/memory_core/retrieval/handle.rs:685-...`) is a
**genuine, irreversible hard delete**: it removes the vector from the
usearch index, deletes the KG drawer metadata
(`kg.delete_drawer`, `kg/ops.rs:167`), cascade-deletes every KG triple whose
subject is `drawer:<id>` (`kg.cascade_delete_by_drawer`, `kg/ops.rs:239`),
removes the entry from the in-memory `drawers` Vec, and rewrites the L1
on-disk snapshot. There is no soft-delete, no tombstone flag, and no
"archived" `DrawerType` variant anywhere in the codebase.

**Pre-existing correctness gap this work also closes:** the current semantic
consolidation phase already writes a `drawer:<orig> superseded_by
drawer:<canonical>` KG triple for every consolidated original
(`record_provenance_and_collect_superseded`, `cycle.rs:427-456`) — but the
`superseded_ids` this returns are **silently discarded**:

```rust
// cycle.rs:560  [drift: now cycle.rs:561 on origin/main]
let (canonical_count, _superseded) = apply_consolidation_result(handle, &result).await;
```

Nothing ever forgets, hides, or filters on these ids. The original drawers
stay **fully live** — recallable, re-batchable into the next cycle's
snapshot, indexed in the closet keyword index — forever. Today's
consolidation therefore **only ever adds** drawers; it can never shrink a
palace, and `DreamStats::update_compression_ratio` (`config.rs:193-208`)
already has to defensively clamp and `tracing::warn!` on "net palace growth
detected" (`config.rs:197-203`) — which is the expected steady state of the
current implementation, not an edge case.

---

## 3. Proposed design

### 3.1 Pipeline, end to end

```
 idle dream cycle (existing, per-palace, every idle_secs)
   │
   ├─ existing passes unchanged: content-prune → dedup → prune → compact → closets
   │
   └─ NEW: structured_consolidation_pass  (sibling to semantic_consolidation_pass)
        │
        1. cluster selection   — pick a bounded group of live, non-archived,
                                  non-protected drawers (§3.2)
        2. prompt construction — same drawer-list formatting as the existing
                                  `build_consolidation_prompt` (types.rs:159-165),
                                  reused as-is
        3. tool-calling invocation — ChatProvider::chat_stream(messages,
                                  vec![EMIT_CONSOLIDATION_TOOL], tx)  (§3.3)
        4. collect the single `ChatEvent::ToolCall` (or none — see §6)
        5. validate arguments   — parse JSON, reject malformed/partial (§3.4, §6)
        6. storage:
             a. handle.remember(summary, ...)              → new summary drawer
             b. handle.remember(inference, ...) per item    → new AgentNote drawer(s)
                (or fold into the summary drawer — open in §3.4)
             c. handle.kg.assert(Triple{...}) per fact       → new KG triples,
                confidence mapped from the model's reported confidence (§4.2)
             d. for each source drawer id: assert
                drawer:<source> --superseded_by--> drawer:<summary>
                (reusing the EXISTING triple shape from
                record_provenance_and_collect_superseded, cycle.rs:427-456)
        7. NEW: actually tombstone the sources this time — mark them archived
           via the same triple (§4), instead of discarding the ids as
           semantic_consolidation_pass does today (cycle.rs:560)
```

This is deliberately **additive to, not a replacement of**, the existing
`semantic_consolidation_pass`. Both can coexist behind independent config
flags (§5) while the new pass is validated; the old pass's Alias/Flag
actions (which the new tool schema does not need to replicate — see §3.3)
keep working unchanged.

### 3.2 What is a "cluster," concretely

Per §2.2, there is no existing embedding-based grouping to build on — the
`similarity_threshold` field is unused dead config. Two candidate
definitions, left open for Bob in §9:

- **v0 — room snapshot** (minimum-risk, reuses existing plumbing): a cluster
  is "the next `max_batch_size` non-archived, non-protected drawers in one
  room," exactly mirroring how `dream_consolidate_room`
  (`dream_ops.rs:38-77`) already scopes work today, and how
  `SemanticConsolidator::consolidate`'s `chunks(max_batch_size)`
  (`consolidator.rs:75`) already batches. Zero new selection logic; the
  "cluster" is really "the next batch."
- **v1 — embedding-similarity grouping**: actually compute pairwise cosine
  similarity (the palace's `vector_store` already exists for this — see
  `crates/trusty-common/src/memory_core/store/vector.rs`) and group drawers
  above `similarity_threshold` before batching. This is strictly more work
  and finally makes the existing-but-dead config field meaningful, but is
  out of scope for a PoC (§7).

### 3.3 Tool schema

Single tool, one call per cluster, passed as the only entry in `tools:
Vec<ToolDef>` to `ChatProvider::chat_stream`:

```json
{
  "name": "emit_consolidation",
  "description": "Report the result of consolidating a cluster of related memories: a short summary, any additional inferences drawn from reading them together, and any subject-predicate-object facts that can be asserted with confidence.",
  "parameters": {
    "type": "object",
    "properties": {
      "summary": {
        "type": "string",
        "description": "A single, standalone paragraph that captures everything worth keeping from this cluster. Must not lose any fact a reader would need."
      },
      "inferences": {
        "type": "array",
        "items": { "type": "string" },
        "description": "Additional conclusions that follow from reading the cluster together but are not stated in any single source memory (e.g. contradictions noticed, implied relationships). Empty array if none."
      },
      "facts": {
        "type": "array",
        "items": {
          "type": "object",
          "properties": {
            "subject":    { "type": "string" },
            "predicate":  { "type": "string" },
            "object":     { "type": "string" },
            "confidence": { "type": "number", "minimum": 0.0, "maximum": 1.0 }
          },
          "required": ["subject", "predicate", "object", "confidence"]
        },
        "description": "Ontological facts as (subject, predicate, object) triples derivable from this cluster, each with the model's own confidence. Empty array if none."
      }
    },
    "required": ["summary", "inferences", "facts"]
  }
}
```

Invocation: build `messages` the same way `build_consolidation_prompt`
already does (reuse `types.rs:159-165` verbatim — no change needed there),
set `tools: vec![emit_consolidation_tool()]`, call `chat_stream`, and collect
exactly one `ChatEvent::ToolCall` off the channel before `ChatEvent::Done`
(model is expected to call the tool exactly once; see §6 for what happens if
it doesn't).

### 3.4 Model-confidence → `Triple.confidence` mapping

Pass the model's own `confidence` field straight through:
`Triple.confidence = fact.confidence.clamp(0.0, 1.0)` (the field is already
`f32` in `[0.0, 1.0]` per `kg/types.rs:52`, so this is a direct assignment
plus a defensive clamp — no rescaling). `provenance` is set to a new,
distinguishable tag: `"dream:llm_cluster_consolidation"` (fixed by epic
#2866; this supersedes this spec draft's earlier
`"dream:structured_consolidation"` placeholder) vs. the legacy pass's
`"dream:semantic_consolidation"` (`cycle.rs:441`), so the two generations of
consolidation are auditable separately in KG queries.

Open sub-decision (not blocking, can default): whether `inferences[]` become
their own `AgentNote` drawers (one per string) or get folded into the single
summary drawer as an appended section. Default recommendation: fold into the
summary drawer's content (simpler storage, one new drawer per cluster
instead of `1 + len(inferences)`); revisit if operators want inferences
independently recallable/forgettable.

---

## 4. Data model changes — tombstone + provenance back-link

**Decision (settled): reuse the existing `superseded_by` KG triple as the
archive marker; do not add a new `DrawerType` variant or a new `Drawer`
field.**

### 4.1 Why not a new `DrawerType::Archived` variant (rejected)

`DrawerType` is hand-annotated with "SERIALIZATION SAFETY" comments at
`crates/trusty-common/src/memory_core/palace.rs:126-141` precisely because
its variants are positionally encoded in an on-disk **postcard** snapshot
(the L1 cache written by `L1Cache::save_l1_cache`, read back on daemon
restart) — `Task` was deliberately appended at the very end (index 5) to
avoid shifting existing on-disk discriminants. A new `Archived` variant is
technically addable the same way (append at index 6), but it:

- still carries the same class of forward-compat risk this file's own
  comments warn about (any future re-ordering mistake corrupts existing
  palaces silently);
- requires updating `Drawer::new()`, `is_protected()`-style call sites, and
  every place that currently matches on `DrawerType` exhaustively or
  near-exhaustively;
- duplicates state that the KG can already represent with zero schema
  changes.

The `kg_redb::DrawerRecord.drawer_type` field (the *other* on-disk copy, used
for KG-side metadata) is already a plain `Option<String>`
(`crates/trusty-common/src/memory_core/store/kg_redb/types.rs:93-104`,
`:131-146`) — not positionally encoded — but the in-memory `Drawer` struct's
`DrawerType` enum is what actually gates dream-cycle behavior everywhere
(`is_protected()`), and that's the one with the postcard risk. Not worth
the risk for what the KG can already express.

### 4.2 The chosen design: KG-triple tombstone

**No changes to `Drawer` or `DrawerType` at all.** A drawer is *archived* iff
it is the subject of an **active** `superseded_by` triple:

```
drawer:<source_id>  --superseded_by-->  drawer:<summary_id>
  confidence: 1.0
  provenance: "dream:structured_consolidation"   (or "dream:semantic_consolidation" for the legacy path)
```

This is the *exact* triple shape `record_provenance_and_collect_superseded`
already writes today (`cycle.rs:427-456`) — the only change is that this
spec's new pass actually **acts on** the resulting archived-id set instead of
discarding it (§2.5). Provenance back-link direction: to answer "what
sources back this summary drawer," use `KnowledgeGraph::incoming` (already
implemented per the module doc at `kg/graph.rs:1-9`) filtered to predicate
`superseded_by` on `drawer:<summary_id>` — **no second, reverse triple is
needed**; the existing directed edge plus the existing `incoming` traversal
already answers the reverse query. This keeps the change purely additive to
data already flowing through a tested code path.

Before/after:

```
BEFORE (today):
  drawer A (live) ──┐
  drawer B (live) ──┼─ superseded_by ──> drawer C (canonical, live)
                     │   (written, but ids discarded — cycle.rs:560)
  A, B remain fully recallable, re-clustered, indexed forever.

AFTER (this spec):
  drawer A (live, tombstoned) ──┐
  drawer B (live, tombstoned) ──┼─ superseded_by ──> drawer C (canonical, live)
                                 │  provenance: "dream:structured_consolidation"
  A, B: excluded from recall / dedup / re-clustering / closet index by the
        new archived-filter (§4.3); content + KG history fully intact;
        recoverable by construction (nothing was deleted).
```

### 4.3 The honest cost: recall-path threading

Every dream/recall code path today filters purely on the in-memory
`Drawer.drawer_type.is_protected()` — a field access, zero I/O. Excluding
archived drawers means each of the following call sites must now also
consult the KG (an async, I/O-bound store) rather than only scanning the
in-memory `Vec<Drawer>`:

| Call site | File:line | Change needed |
|---|---|---|
| `semantic_consolidation_pass` snapshot filter | `cycle.rs:548-554` | add `&& !archived.contains(&d.id)` alongside the existing `!d.drawer_type.is_protected()` filter |
| new `structured_consolidation_pass` cluster selection | new, §3.2 | same filter, from the start |
| `content_prune_pass`, `dedup_pass`, `prune_pass` | `cycle.rs` | skip archived drawers — they're already superseded, re-evaluating wastes cycle budget |
| `refresh_closets` / `build_closet_index` | `helpers.rs` (per earlier search), `cycle.rs:289-296` | exclude archived so keyword search doesn't surface tombstoned content |
| general recall ranking | `crates/trusty-common/src/memory_core/retrieval/*` | exclude archived from ranked results (the main user-facing cost — a stale drawer surfacing in `memory_recall` defeats the point of consolidating it) |
| `memory_list` MCP tool | `crates/trusty-memory/src/tools/*` (list handler) | default-exclude; add an `include_archived: bool` param mirroring `task_list`'s existing `include_completed` pattern |
| fading-memories pass (once #2352 lands on this branch) | `fading.rs` (not yet present here — §2.1) | exclude archived from fading candidates too |

Mitigation for the I/O cost: do **not** query the KG per-drawer per-pass.
Preload the full archived-id set **once per dream cycle** (or once per
recall call) via a single bulk KG scan, cached for the duration of that one
cycle/call. This requires one small, new addition to
`crates/trusty-common/src/memory_core/store/kg/ops.rs`: a
`subjects_for_predicate(predicate: &str) -> Result<HashSet<String>>`-shaped
helper (no existing method returns "all subjects with predicate X" in bulk —
`list_subjects(limit)` returns all active subjects regardless of predicate).
This is the one genuinely new piece of KG-store surface this spec requires;
everything else in §4 reuses existing methods.

---

## 5. Configuration

- **Enable flag — default OFF for the PoC:** new
  `SemanticConsolidationConfig`-sibling config, e.g.
  `StructuredConsolidationConfig { enabled: bool /* default false */, model:
  String, max_batch_size: usize, max_calls_per_cycle: usize }`, living beside
  `SemanticConsolidationConfig` in
  `crates/trusty-common/src/memory_core/semantic_consolidation/types.rs` (or
  a new sibling module — see §7/§8). Threaded onto `DreamConfig` the same way
  `semantic: SemanticConsolidationConfig` already is (`config.rs:41`).
- **Provider:** reuse `AppState::chat_provider()`
  (`crates/trusty-memory/src/lib.rs:1260`) exactly as `trusty-memory`'s own
  chat feature does — no new provider construction path.
- **Model / API key source:** `~/.trusty-memory/config.toml`
  `[openrouter]` (`api_key`, `model`) / `[local_model]`
  (`enabled`, `base_url`, `model`) — same file, same section names as today
  (§2.3). No new env vars or config keys needed beyond a
  `[dream.structured_consolidation] enabled = false` (or equivalent) toggle.
- **Cost/rate controls:** mirror the existing, already-proven pattern:
  `max_batch_size` (cluster size cap) and `max_calls_per_cycle` (hard ceiling
  on LLM calls per dream cycle) exactly as `SemanticConsolidationConfig`
  already enforces them today (`consolidator.rs:75-82`, budget check at
  `consolidator.rs:76-82`). Reuse the same response cache pattern
  (`batch_cache_key`, `types.rs:234-245`) so repeated identical clusters
  don't re-spend calls.
- **Fail-open, mandatory:** when no provider is configured
  (`AppState::chat_provider()` returns `None`), the new pass must return
  immediately with a zero-effect result — exactly the existing no-op
  contract `inference_available` already guarantees for the legacy pass
  (`semantic_consolidation/types.rs:139-150`, and proven by the existing
  test `dream_cycle_semantic_consolidation_no_inference`,
  `crates/trusty-common/src/memory_core/dream/tests.rs:769-810`, and the MCP
  end-to-end test `palace_dream_no_inference_returns_gracefully`,
  `crates/trusty-memory/tests/dream_room_mcp.rs:83-108`). The rest of
  `dream_cycle` (dedup, prune, compact, closets) must run unaffected whether
  or not this pass fires — it must never be able to error the daemon or
  abort the cycle.

---

## 6. Failure modes and safety

This pass mutates durable user memory (creates drawers, asserts KG triples,
tombstones sources) — every failure mode below assumes partial completion is
possible and must be handled without corrupting palace state.

| Failure | Mitigation |
|---|---|
| No provider configured | Fail open — pass no-ops, rest of dream cycle unaffected (§5). Proven pattern, not new. |
| Model never calls the tool (returns plain text instead) | Treat as zero actions for that cluster, same graceful-degradation the legacy `parse_consolidation_actions` already applies on parse failure (`types.rs:212-222`) — log at `debug`, move to the next batch, do not retry within the same cycle. |
| Model calls the tool with malformed/incomplete JSON arguments | `ToolCall.arguments` is a raw `String` by design (`chat/mod.rs:116-119`, specifically so callers can inspect/log malformed JSON rather than getting a silent parse failure inside the provider) — validate against the schema before acting; on failure, log the raw arguments at `warn` and skip the cluster. Never partially apply a malformed result. |
| Hallucinated / low-confidence facts | Model-reported `confidence` flows straight into `Triple.confidence` (§3.4) — nothing is filtered at write time (matches existing `kg_assert` behavior, which accepts any confidence a caller supplies), but callers of `kg_query`/recall can threshold on it. Consider (not blocking the PoC): a config-level minimum-confidence floor below which a fact is dropped rather than asserted — left for a follow-up if hallucination rate proves high in practice. |
| Cost runaway | `max_calls_per_cycle` hard ceiling (existing pattern, §5) plus `max_batch_size` capping cluster size; response cache avoids re-paying for identical batches across cycles. |
| Partial failure mid-cluster (e.g. summary drawer written, then daemon crashes before the tombstone triples are asserted) | Order operations so the **tombstone is the last write**, not the first: create the summary drawer + assert facts first, tombstone sources last. Worst case on a crash: a summary drawer exists but sources are *not yet* archived — sources stay fully live and recallable (safe, just redundant — no data loss, no dangling reference). Never assert `superseded_by` before the summary drawer it points to exists. |
| Idempotency / re-entrancy if a cycle is interrupted and re-run | A drawer already tombstoned (`superseded_by` triple present, §4.3) must be excluded from cluster selection on the next cycle (§4.3's filter) — so a re-run cannot re-summarize already-archived sources. If the crash happened *before* the tombstone write (previous row), the source is simply picked up fresh next cycle and re-clustered/re-summarized — produces a second summary drawer, not corruption; acceptable duplication, not a correctness bug. |
| Recall regression (tombstoned drawer still surfacing) | This is the §4.3 threading list — until every listed call site is updated, do not turn `enabled` on by default. This is the primary reason this ships default-OFF (§5, §7). |

---

## 7. PoC scope vs. future work

**The PoC proves:** a cheap tool-calling model, invoked via the *existing*
`ChatProvider` abstraction (no new provider code), can read a bounded cluster
of memories and return a structured `{summary, inferences[], facts[]}` block
via one real tool call; the result is stored as a new drawer + new KG
triples; the sources are tombstoned via the existing `superseded_by` triple
shape and excluded from at least recall + re-clustering (the two
highest-value entries in §4.3's table). End-to-end, this is default-off,
opt-in per palace/room via the existing `dream_consolidate_room` on-demand
tool, so it can be validated against a real palace without touching the
autonomous idle scheduler at all.

**The PoC deliberately does not do:**
- embedding-based cluster selection (§3.2 v1) — v0 room-snapshot batching only;
- thread the archived-filter through *every* row in §4.3's table — PoC
  targets recall + re-clustering only; closets/fading/`memory_list` follow
  as fast-follow work once the pattern is validated;
- a confidence floor / auto-rejection of low-confidence facts (§6);
- independent recall/forget of individual `inferences[]` entries (folded
  into the summary drawer, §3.4);
- wiring into the autonomous idle scheduler by default — PoC is on-demand
  (`dream_consolidate_room`) only; autonomous default-on is a deliberate
  later decision once the PoC's output quality is reviewed by a human.

---

## 8. Work decomposition

Suggested filing order under one epic; each sized independently.

1. **KG bulk-subject helper** (S) — add
   `subjects_for_predicate(predicate) -> Result<HashSet<String>>` (or
   equivalent) to `crates/trusty-common/src/memory_core/store/kg/ops.rs`.
   No callers yet; unblocks #2 and #4. Pure addition, no existing behavior
   touched.
2. **Tool schema + structured consolidator module** (M) — new sibling module
   to `semantic_consolidation/` (e.g.
   `crates/trusty-common/src/memory_core/structured_consolidation/`):
   `emit_consolidation` `ToolDef` builder, `ChatProvider`-based invocation
   (reusing `build_consolidation_prompt`), response validation, and the
   `StructuredConsolidationConfig` type. Unit-testable in isolation with a
   stub `ChatProvider` (mirrors the existing `MockInference` pattern at
   `inference.rs`). Depends on: nothing new besides #1 for filtering during
   its own cluster selection.
3. **Archived-filter threading: recall + re-clustering only** (M) — thread
   the §4.3 filter through the two highest-value call sites (general recall
   ranking, and the new pass's own + the legacy pass's cluster snapshot
   filter at `cycle.rs:548-554`), using #1's bulk helper cached per
   cycle/call. Depends on: #1.
4. **Wire the new pass into `dream_consolidate_room` / `palace_dream`
   on-demand path only** (S) — extend `handle_dream_consolidate_room`
   (`dream_ops.rs:38-77`) to optionally run the new pass alongside (not
   instead of) the existing `consolidate_scoped` call, gated by the
   default-off config flag from #2. Depends on: #2, #3.
5. **Tombstone-on-completion + provenance back-link wiring** (S) — the part
   of §3.1 step 7/§4.2 that actually asserts the `superseded_by` triple for
   the new pass's sources (reusing `record_provenance_and_collect_superseded`'s
   shape) and fixes the ordering described in §6 (tombstone last). Depends
   on: #2.
6. **Fast-follow: `memory_list`/closets archived-filter + confidence floor
   config** (M) — the remainder of §4.3's table, plus the optional §6
   confidence floor. Explicitly deferred past the PoC (§7); files as a
   separate follow-up ticket, not blocking PoC sign-off.

---

## 9. Decisions — RESOLVED (Bob, 2026-07-16)

Both formerly-open decisions are settled and implemented in the epic #2866
PoC:

1. **Cheap model id default: `anthropic/claude-haiku-4-5` — DECIDED
   2026-07-16.** The tool-calling pass reuses the same default the legacy
   free-text pass already ships (`semantic_consolidation/types.rs:50`), for
   consistency and cost-familiarity. Implemented as
   `DreamConsolidationConfig::default().model` in
   `crates/trusty-common/src/memory_core/dream_consolidation/types.rs`.
   `ChatProvider`/`OpenRouterProvider` remain model-agnostic, so a different
   model is a one-line config change (`[dream_consolidation] model = "..."`
   in `~/.trusty-memory/config.toml`) if Haiku's tool-calling reliability on
   this schema proves insufficient in practice.
2. **Cluster definition for v0: room snapshot — DECIDED 2026-07-16.** §3.2's
   v0 room-snapshot batching ships: the pass groups the filtered drawer
   snapshot by room and chunks each room by `max_batch_size` — zero new
   selection logic, mirroring the existing `chunks(max_batch_size)`
   precedent. Embedding-similarity clustering (§3.2 v1) is explicitly
   deferred to future work (§7); the dead `similarity_threshold` config
   field remains unused until that work lands.
