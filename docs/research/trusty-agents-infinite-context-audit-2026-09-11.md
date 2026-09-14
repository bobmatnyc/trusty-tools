# trusty-agents "infinite context" — spec vs. implementation audit

**Date:** 2026-09-11
**Scope:** `crates/trusty-agents` (assistant runtime + UI) at `origin/main` (commit
`48ccae7d4a2cb779a924c67578588c7639f100d5`), read-only.
**Method:** `git show origin/main:<path>` / `git grep origin/main -- <path>` from a
worktree on a different branch; no files edited; no issues filed.

## 0. Scope-correction — three different "infinite context" designs exist in this repo

Before R1–R7 can be checked, one conflation has to be resolved: the owner's known
starting points name THREE architecturally separate subsystems, and only one of
them is `crates/trusty-agents`.

| Subsystem | Where | Status |
|---|---|---|
| **tcode "Infinite Sessions"** — goal slots (5 slots, `set_goal`/`clear_goal`), a working-context budget, round-based compaction | `docs/specs/trusty-code-harness-ui.md` (DOC-39/SPEC-TCUI-05/07), epic **#2343** | Built for **trusty-code (tcode)**, a PM/coding-harness session inside trusty-mpm. Epic #2343 and its sub-issues (#2347 goal slots, #2350 API surface, #2368 hardening) are all **CLOSED**. Lives in `crates/trusty-code`-adjacent surfaces (`session/protocol_goals.rs`, `tools/goals.rs`), **not** in `crates/trusty-agents`. |
| **trusty-mpm "session manager (SM)" agent** — DOC-14, a 10-round verbatim window + growing compressed summary, Haiku-tier `summary_model` | `docs/specs/session-manager-agent.md` §7 | **Draft, unbuilt** ("does **not** implement anything… opens **no** Rust changes" — session-manager-agent.md:23-24). Scoped to `trusty-mpm`'s own coordinator/PM chat brain, not the trusty-agents assistant. Lists `crates/trusty-agents/src/llm/anthropic_native/` only as *prior art to reuse*, not as itself. |
| **trusty-agents' own workstream-classification model** — DOC-54 §9.6 (`trusty-agents-product-spec.md`), implemented in `crates/trusty-agents/src/ctrl/pm_task/dispatch/classification.rs` + `persona_memory.rs` | This spec | **This is the one the owner's chat sessions actually run.** Partially built (below). |

Two further legacy documents must be discounted entirely:
`docs/trusty-agents/spec/PRD.md` and `docs/trusty-agents/spec/COMPONENTS.md` are
explicitly headed **"Status: Historical baseline · not a current behavior
contract… This document predates the `trusty-agents` rename"**
(PRD.md:3-8, COMPONENTS.md:3-8). Their `ContextManager`/`soft_threshold`
description (COMPONENTS.md:181, PRD.md:242) is the *open-mpm*-era framing of a
type that still exists today under a different design (Part B.2) — do not read
these two files as current spec for anything.

The rest of this report treats **DOC-54 §9 (`trusty-agents-product-spec.md`)**
as the authoritative current spec for R1–R7, and cross-references DOC-14/DOC-39
only where the owner's language ("N messages kept fresh," "25%/35%," "goal
slots") demonstrably comes from those adjacent, non-trusty-agents designs.

---

## Part A — Spec

| # | Owner statement | Spec says | file:line | Verdict |
|---|---|---|---|---|
| R1 | Each chat session recorded in sidebar as a "Task," label inferred | "User-facing terminology: TASKS… Internal/technical term: Workstreams… Lists **inferred** workstream classifications (agent-inferred task/topic boundaries)." Also: "Name is **inferred** from what you're doing." | `docs/specs/trusty-agents-product-spec.md:417-429`; `docs/specs/trusty-code-harness-ui.md:190` | **AGREES** — but see terminology note below: the spec's "Task" is a *classification/filter* over one continuous per-agent conversation, not a separate chat session object. |
| R2 | Infinite context: consolidated past + N fresh verbatim + primary/secondary goals | "Global prompt-history summary… always present" + "Per-workstream summary… every N turns" + "`recent_window`: raw turns sent in focused mode" (`trusty-agents-product-spec.md:529-540`). **No mention of "primary goal / secondary goals" anywhere in DOC-54.** Goal slots are a *tcode* concept (`trusty-code-harness-ui.md:194,225-258`), not part of the trusty-agents spec. | `docs/specs/trusty-agents-product-spec.md:521-566` | **DIFFERS + SILENT** — consolidation/window shape agrees; goal-tracking is SILENT for trusty-agents (it's specified only for tcode, a different subsystem). |
| R3 | State (consolidated history, fresh window, goals) stored in trusty-memory, tagged with unique session ID + label | "The continuous conversation is persisted in trusty-memory… Workstream classifications are metadata within that conversation history" (`trusty-agents-product-spec.md:517`). No explicit "session ID" concept in DOC-54 — the session model is "ONE continuous conversation per agent" (`:485`), not one session per Task. | `docs/specs/trusty-agents-product-spec.md:483-485,517` | **DIFFERS** on granularity: spec ties persistence to the agent, not to a per-chat-session ID; "Task" is a tag inside that one persisted stream, not a separate tagged session. |
| R4 | Full session history logged durably, nothing lost | "All agent memories are accessible at all times; there is no context walling or clearing… '+ New Task' does not clear context… 'Clear Context' removed: this concept does not exist" (`trusty-agents-product-spec.md:485,500-505`). | `docs/specs/trusty-agents-product-spec.md:485,500-505` | **AGREES.** |
| R5 | Only consolidated history + last N sent to LLM | "Context assembly (stable order): Global summary… Per-workstream summary (if focused)… N recent prompts of the filtered type… Memory tools always live" — **but this order is explicitly scoped to *focused mode***; unfocused mode sends "N recent prompts of all types" (`trusty-agents-product-spec.md:546-553`). | `docs/specs/trusty-agents-product-spec.md:546-558` | **AGREES for focused mode**; spec is **SILENT/DIFFERS** for the unfocused/default case — it says "N recent prompts of all types," not "consolidated summary only." |
| R6 | Consolidation triggers at ~25% of context window, redone at ~35%, both configurable | DOC-54 uses a **turn-count** cadence, not a percent-of-context trigger: "`summarize_every`: N-turn cadence for per-workstream summary regeneration" (`:536-537`). No 25%/35% or any percent-of-context-window figure appears anywhere in `docs/specs/**` or `docs/trusty-agents/**` for trusty-agents (`git grep` for `0.25`/`0.35`/`25%`/`35%` across those trees returns no trusty-agents hit; the only percent-of-context design in the whole repo is DOC-39/tcode's `overhead_cap_tokens = context_window * 40/100`, `trusty-code-harness-ui.md:411`, a single 40% cap, not a 25/35 pair, and not for trusty-agents). DOC-14's SM-agent draft (unbuilt, different subsystem) also uses round-count: "Round-count is the primary trigger per the brief" (`session-manager-agent.md:784`) with a flat token-budget safety valve (`context_token_budget = 24000`, `:984`), not a percent. | `docs/specs/trusty-agents-product-spec.md:536-537`; `docs/specs/session-manager-agent.md:784,984` | **DIFFERS** — every written spec that covers a trigger uses round-count or a flat token budget or a single 40% cap; none specifies a 25%-in/35%-redo percent pair. This looks like the owner's own mental model, not a written requirement. |
| R7 | Consolidation by an inexpensive LLM, asynchronously | DOC-54 does not name a model tier for the per-workstream summary call at all. DOC-14 (different subsystem, unbuilt) is explicit: "Compaction cost & quality… Summarization + compaction calls use the cheaper `summary_model` (Haiku tier)" (`session-manager-agent.md:658,671,688-690`). Async is not discussed as a requirement in DOC-54; DOC-14 does discuss compaction cost bounding (`:808`) but not concurrency/blocking. | `docs/specs/session-manager-agent.md:658,671,688-690` | **SILENT for trusty-agents** on both the cheap-model requirement and the async requirement; the "inexpensive LLM" requirement is written only for the *other* subsystem (DOC-14). |

**Terminology note (applies to R1/R3):** `docs/specs/DOC-52-shared-workstream-definition.md:106` explicitly warns: *"Not to be confused with trusty-memory's Task drawer… `task_add`/`task_list`/`task_complete`… That is a genuinely different, unrelated use of the word 'task'… Do not unify the two."* The owner's R1 wording ("recorded... as a Task") and trusty-memory's own `task_add`/`task_list` MCP tools are a distinct, unrelated feature per this ADR-level ruling — worth flagging since the audit's own tool list includes `mcp__trusty-memory__task_add`/`task_list`/`task_complete`, which are NOT what powers the trusty-agents sidebar.

---

## Part B — Implementation (live chat-turn trace)

### B.1 The turn path that actually backs the GUI/Slack/Telegram chat

`crates/trusty-agents/src/ctrl/pm_task/dispatch/persona.rs::run_pm_task_with_persona`
is the one entry point used by the GUI (`api/server/handlers.rs:516`), Slack
(`slack/handlers.rs:542`), Telegram (`telegram/handlers.rs:404`), and event
listeners (`listeners/wake.rs:307`). Critically, **the GUI's own submission call
passes an empty history slice**:

```
crate::ctrl::run_pm_task_with_persona(&project_path, &agent_name, &task_text, &[], Some(id_bg.clone()), overrides_bg)
```
— `crates/trusty-agents/src/api/server/handlers.rs:516-522` (the `&[]` is the
`history: &[ConversationTurn]` parameter).

`ConversationTurn` itself (`crates/trusty-agents/src/ctrl/state.rs:36`) is
documented as REPL-owned, in-process, non-durable state: *"The REPL owns a
`Vec<ConversationTurn>` and forwards it to `run_pm_task_with_history` on every
task submission"* (`state.rs:31-33`). The GUI/API path does not use this
mechanism at all for persona chat — it is only exercised by the CLI REPL
(`run_pm_task_with_history`, a **different** dispatch function from the persona
one).

So for the product's actual chat surface, there is **no verbatim rolling window
of prior raw turns supplied automatically on every call** the way R2/R5 assume.
Continuity instead comes from two independent, narrower mechanisms:

### B.2 Send-time context-window safety net — deterministic, not consolidation

`crates/trusty-agents/src/context/manager.rs:70-77` — `ContextManager` holds
`soft_threshold: f32` (fraction of the model's context window) and a `budgets`
field explicitly marked `#[allow(dead_code)]` with the comment "unused today."
Instantiated once, hardcoded, in the live tool loop:

```
let ctx_manager = ContextManager::new(0.5);
```
— `crates/trusty-agents/src/llm/tool_loop/mod.rs:239`, then applied via
`trim_messages_with_manager` at `:251` before every LLM call
(`crates/trusty-agents/src/llm/compress.rs:36-58`).

`trim_to_budget` (`crates/trusty-agents/src/context/trim.rs:32-108`) is **pure
eviction/truncation** — "water-fill truncate the OLD region," then "evict oldest
OLD messages," then truncate the recency window as a last resort. It calls no
LLM and produces no summary; content that doesn't fit is **dropped or cut**, not
consolidated. `soft_threshold` is a single fixed value (0.5 = 50%), hardcoded at
the one call site, with no corresponding TOML field on `AgentCompressConfig` or
anywhere else (`git grep AgentCompressConfig` / `soft_threshold` under
`crates/trusty-agents/src/agents/` returns no config wiring) — **not
configurable**, and not a two-tier 25%/35% pair.

### B.3 `SessionCompressor` / `SessionCompressionConfig` — built, tested, never wired

`crates/trusty-agents/src/compress/session.rs:79-91` defines exactly the shape
R2/R6/R7 describe: `SessionCompressor { threshold: 40, keep_recent: 10,
summary_model: None }`, with `should_compress()` (turn-count based) and an
async `compress()` that calls an `LlmClient` to produce one `"[CONVERSATION
SUMMARY]"` system message, keeping `keep_recent` turns verbatim. It has its own
config surface, `SessionCompressionConfig` (`crates/trusty-agents/src/agents/
params.rs:145-172`): `enabled` (default **false**), `compression_threshold`
(40), `keep_recent_turns` (10), `compression_model: Option<String>` — doc
comment literally says *"cheap model recommended"* (`params.rs:141`).

**Neither type is ever read outside its own definition, its own unit tests, and
one eval fixture.** `git grep SessionCompressor` across `crates/trusty-agents/**`
returns only: the struct/impl itself, its own tests, and
`evals/cto-assistant.toml:54-69`, which tests that the CTO-Assistant *agent's own
documentation retrieval* can answer "where is SessionCompressor defined" — i.e.
a doc-recall eval, not a wiring test. `SessionCompressionConfig` is likewise
only ever constructed via `::default()` in five unrelated call sites
(`claude_code_runner/tests.rs:103`, `claude_mpm_loader.rs:158`, `mpm_bridge.rs:
389`, `md_agent.rs:228`) and never read (`.session.` field access does not
appear anywhere in non-test runtime code). **This is the module that most
closely matches R2/R6/R7's described mechanism, and it is dead code on every
live chat path.**

### B.4 What actually implements the "infinite context" feel: workstream classification

`crates/trusty-agents/src/ctrl/pm_task/dispatch/classification.rs` (module doc,
lines 1-70) is DOC-54 §9.6's real implementation, called from `persona.rs`'s two
return paths via exactly two entry points:

- **`build_turn_context`** (`classification.rs:263-300`) — before the system
  prompt is built. When `[workstreams].enabled` (real master switch, default
  `true`, `params.rs:210-212`) it fetches the closed label vocabulary and
  renders the classification instruction block; **only when a workstream is
  currently focused** does it call `assemble_focused_block`
  (`classification.rs:305-352`), which assembles, in the spec's stable order:
  a hardcoded **stub** string for "global summary" (*"global summary not yet
  wired in this slice"* — `classification.rs:314-317`), the cached
  `ws-summary:<label>` drawer, then the last `recent_window` (default 12,
  `params.rs:219-221`) raw turns tagged `ws:<label>`. **When unfocused, none of
  this runs** — `focused_context_block: None` (`classification.rs:236-244`).
- **`finish_turn`** (`classification.rs:391-461`) — parses the trailing
  `[[task: <label>]]` marker the model is instructed to emit (this is R1's
  "inferred, not typed" label), applies a "task bleed" nudge if the
  classification drifts from the focused workstream, returns the display text
  **immediately**, then `tokio::spawn`s (`classification.rs:448`) a detached
  background task that (a) persists the turn as a `ws:<label>`-tagged drawer
  and (b) calls `maybe_summarize_workstream`.
- **`maybe_summarize_workstream`** (`classification.rs:506-560`) — cadence
  check is `should_refresh_summary` (`:496-501`, pure, turn-count modulo
  `summarize_every`, default 5 — `params.rs:215-217`), **not** a percent of
  context window. The summarization call is:
  ```
  llm::chat_adapter_aware(client, &persona_cfg.agent.model, system, &joined, 0.2, 400, Vec::new())
  ```
  — `classification.rs:544` — **`persona_cfg.agent.model`, the SAME model the
  persona itself uses**, not a separate cheap/Haiku model. The module doc says
  this explicitly: *"one extra LLM call on the SAME credentials/model"*
  (`classification.rs:36-38`).

**R1 — IMPLEMENTED.** `[[task: <label>]]` marker parsing
(`classification.rs:118-160` for `parse_marker`/`is_valid_label`), persisted as
a `ws:<label>` tag, surfaced at `GET /api/workstreams`
(`crates/trusty-agents/src/api/server/workstreams.rs`) and rendered by the GUI
sidebar (`crates/trusty-agents/ui/src/lib/workstreams.ts:1-14,39-46`). Tests:
`is_valid_label_accepts_kebab_case`, `is_valid_label_rejects_placeholder_and_
unsafe_text`, `build_turn_context_unfocused_has_no_context_block`,
`build_turn_context_focused_assembles_stable_order`,
`build_turn_context_disabled_is_a_real_no_op` (module doc test list,
`classification.rs:65-70`; bodies in `classification_tests.rs`).

**R2 — PARTIAL.** Consolidation + fresh window exist, but only in *focused*
mode, and the "global summary" half is a labeled stub with no real content
(`classification.rs:314-317`). In *unfocused* mode (the default state before a
user clicks a task) there is no automatic prior-turn injection at all from this
module, and — per B.1 — the GUI's dispatch call also supplies no
`ConversationTurn` history. Continuity in the unfocused/default case comes only
from `persona_memory.rs`'s semantic recall (below), which is RAG-style
similarity search, not a verbatim recent-window. **Primary/secondary goal
tracking is MISSING** — `git grep -i 'goal'` across `crates/trusty-agents/**`
(excluding UI lockfiles/vendor) returns zero hits; goal slots exist only in
tcode (`crates/trusty-code`-adjacent, a different product surface, confirmed
CLOSED issues #2347/#2350/#2368).

**R3 — PARTIAL.** State is persisted to trusty-memory, but "unique session ID"
means one *deterministic, agent-scoped* id — `session_id_for(agent_name) =
format!("persona-{agent_name}")` (`persona_memory.rs:176-183`) — not one ID per
chat session/Task. `chat_session_create` is deliberately idempotent on this
caller-chosen id so **every** persona has exactly ONE continuous trusty-memory
chat session, matching DOC-54 §9.1's "ONE continuous conversation per agent," but
diverging from the owner's "unique session ID" framing (which implies one ID per
Task/chat, i.e. per R1's sidebar entry). Label tagging (`ws:<label>` /
`ws-summary:<label>`) IS present per-turn (`classification.rs:378,441-447`;
`workstreams::workstream_tag`/`workstream_summary_tag`).

**R4 — IMPLEMENTED for the default configuration**, with one real caveat.
`persona_memory.rs::spawn_persist_turn_with_activity`
(`persona_memory.rs:604-663`) fires on every turn (detached, fail-open, file-locked
against concurrent writers via `acquire_chat_persistence_lock`,
`persona_memory.rs:548-576`), calling `chat_session_create` then
`chat_turn_append` / `chat_session_add_turn`
(`persona_memory.rs:701-732,689-698`) — this is the full durable log, separate
from and unconditional relative to the workstream-tagging path. **Caveat:**
`classification::finish_turn` explicitly skips its OWN persistence (the
`ws:<label>`-tagged drawer used for focused-mode recent-window assembly) when
`[workstreams].enabled = false` (`classification.rs:429-436`) — so with
workstreams disabled, the full-transcript log (`persona_memory.rs`) still
survives, but the classification-tagged copy that focused mode reads does not.
Also caveat: `git_issue #4278` ("Chat view is never rehydrated from the durable
message log," CLOSED) confirms this log's UI-rehydration path was itself a past
gap, now fixed — the durable write existed before the UI could read it back.
Test: `persist_turn_creates_session_then_appends`, `spawn_persist_turn_is_noop_
without_a_socket` (module doc, `persona_memory.rs:544-546`).

**R5 — DIFFERS from spec-as-implemented.** What is actually sent to the LLM on
an ordinary (unfocused) persona turn is: system prompt + classification-
vocabulary block + `persona_memory`'s identity/recall block + the single new
user message — **no consolidated summary AND no verbatim recent turns** for the
default case (B.1, B.4). Only in focused mode does the assembled block match
R5's shape (summary + N recent). The `ContextManager` 50% trim (B.2) is the only
thing that runs unconditionally on every call, and it trims/evicts rather than
consolidating.

**R6 — IMPL-GAP, confirmed.** No percent-of-context-window trigger exists
anywhere in trusty-agents. `ContextManager.soft_threshold` is the only
percent-based number in the crate and it is a fixed 0.5 emergency ceiling
(B.2), not a two-stage 25%/35% consolidation trigger, and not configurable. The
actual consolidation cadence that exists (`summarize_every`, `params.rs:215`)
is turn-count-based, matching DOC-54's own written spec (§9.6.2) rather than
the owner's percent framing — i.e. the CODE correctly implements the WRITTEN
spec; it is the owner's R6 statement that has no written spec backing it (see
Part A, R6).

**R7 — PARTIAL.** Async: **confirmed true** — `finish_turn` returns the display
text before spawning persistence/summarization (`classification.rs:437-461,
448`); this is a real, tested non-blocking design (module doc explicitly frames
it as fixing a prior blocking defect, "critic HIGH-4"). Inexpensive model:
**confirmed false** — the summarization call (`classification.rs:544`) uses
`persona_cfg.agent.model`, i.e. whatever model the persona itself runs (per
DOC-54's provider-policy default, that's Sonnet via OpenRouter, not Haiku).
`SessionCompressionConfig.compression_model` (the field that WOULD let an
operator name a cheap override model) exists but is wired to nothing (B.3).

---

## Part C — Gap table

| # | Owner statement | Spec says | Code does | Verdict | Smallest change to close |
|---|---|---|---|---|---|
| R1 | Task = inferred label in sidebar | AGREES (`product-spec.md:417-429`) | Implemented: `[[task: <label>]]` marker → `ws:<label>` tag → `/api/workstreams` → sidebar (`classification.rs:118-160`, `workstreams.ts:39-46`) | **MATCH** | None needed. |
| R2 | Consolidated + N fresh + goals | Consolidated+window AGREES for focused mode only; goals SILENT | Focused mode: real (stub global summary + cached per-WS summary + N raw turns). Unfocused mode: none. Goals: absent. | **BOTH** | Wire a real global-summary source (remove the stub, `classification.rs:314-317`); decide whether goal-tracking is in scope for trusty-agents at all (currently a tcode-only concept) — if yes, port a minimal `[[goal: ...]]` marker analogous to `[[task: ...]]`. |
| R3 | Unique session ID + label tag in memory | DIFFERS (one continuous session per AGENT, not per Task) | Matches spec exactly: `session_id_for(agent) = "persona-{agent}"` (`persona_memory.rs:176`) + per-turn `ws:<label>` tag | **SPEC-GAP** (code matches its own spec; owner's mental model wants per-Task session IDs the spec never asked for) | If per-Task session IDs are actually wanted, that is a new requirement, not a bug — needs a spec decision first. |
| R4 | Full history logged, nothing lost | AGREES | Implemented unconditionally via `persona_memory.rs::spawn_persist_turn_with_activity` → `chat_session_create`/`chat_turn_append` (`persona_memory.rs:604-732`); fail-open on daemon-down (logged, not fatal) | **MATCH**, with a noted caveat: the *separate* `ws:<label>` copy `finish_turn` writes is skipped when `[workstreams].enabled=false` (`classification.rs:429-436`), affecting focused-mode recall but not the durable log itself | Document that the two persistence writers (`persona_memory` full log vs. `classification` workstream-tagged copy) are independent, so a future reader doesn't assume one implies the other. |
| R5 | Only consolidated + last N sent to LLM | AGREES for focused mode; SILENT/DIFFERS for unfocused | Unfocused: no consolidated summary, no verbatim window — just classification vocabulary + semantic recall + the new message. Focused: matches. Additionally, `ContextManager` (50% hard trim, `tool_loop/mod.rs:239`) is the only thing that runs on every call, and it evicts/truncates rather than consolidating. | **BOTH** | Either (a) make focused-mode-shaped assembly the default (always assemble global+WS summary+recent window, not gated on `focused.is_some()`), or (b) wire the already-built `SessionCompressor` (B.3) as the always-on fallback so an unfocused turn still gets a real consolidated-history + verbatim-tail shape instead of relying solely on RAG recall. |
| R6 | 25%-trigger / 35%-redo, configurable | DIFFERS — no written spec anywhere specifies a percent pair; DOC-54 specifies turn-count (`summarize_every`); DOC-39/tcode specifies a single flat 40% cap; DOC-14 specifies round-count + flat token budget | Turn-count cadence (`summarize_every`, default 5) for the WS summary; fixed 0.5 (50%) non-configurable hard trim for the context-window safety net. No percent-pair anywhere. | **BOTH** (spec never asked for this; code doesn't do it either) | If the owner wants a percent-of-context trigger, this needs a NEW spec line in DOC-54 §9.6.2 before it's an implementation task — right now there's nothing in the codebase to "fix" toward this number, only a design decision to make. |
| R7 | Cheap LLM, async | Async AGREES with nothing written (DOC-54 silent, but code does it); cheap-model requirement is written only for the *other* subsystem (DOC-14 `session-manager-agent.md:658`) | Async: real (`tokio::spawn`, `classification.rs:448`). Cheap model: `persona_cfg.agent.model` (same as persona) at the one live call site (`classification.rs:544`); the field that WOULD carry a cheap override (`SessionCompressionConfig.compression_model`) is parsed but never read (B.3). | **BOTH** | Read `SessionCompressionConfig.compression_model` (or add an equivalent field to `WorkstreamContextConfig`) and pass it as the `model` argument at `classification.rs:544` instead of `persona_cfg.agent.model`, defaulting to a Haiku-tier id when unset. |

---

## Part D — Related open issues

Searched `gh issue list --state all --search "<term>"` for: infinite context,
consolidat*, workstream label, Task sidebar, goal slot, summarize_every,
compaction, SessionCompressor.

**Already covers a gap found here (CLOSED):**
- **#3928** — *"Persona chat never consults bound palace/OKG memory — agents
  self-report as stateless despite live bindings (infinite-context gap)"* — the
  issue `persona_memory.rs` (B.4) was built to fix; its module doc quotes the
  exact symptom ("I don't have memories that persist across conversations").
- **#3843** — *"trusty-agents: filterable-context classification — vocabulary
  cap + cadence-freeze bug"* — a `summarize_every`-cadence bug in this same
  module, closed.
- **#4278** — *"Chat view is never rehydrated from the durable message log"* —
  UI-side counterpart to R4's durable log, closed.
- **#2343 / #2347 / #2350 / #2368** — the tcode "Infinite Sessions" epic and its
  goal-slot sub-issues. All CLOSED, all scoped to tcode, not trusty-agents —
  confirms goal-slot work has never targeted this crate.
- **#3867** — *"trusty-code+trusty-agents: structured compression-telemetry
  JSONL instrumentation (Slice A)"* — telemetry for compression events, closed;
  does not address the model-tier or percent-trigger gaps.

**No open issue found tracking:**
- Wiring `SessionCompressor`/`SessionCompressionConfig` into any live chat path
  (B.3) — searched "wire SessionCompressor", "SessionCompressor" directly:
  zero results either way.
- A percent-of-context-window consolidation trigger for trusty-agents
  (searched "context percent", "soft_threshold", "context budget percent"):
  zero results.
- Routing the workstream-summary LLM call through a cheap/Haiku model instead
  of `persona_cfg.agent.model`.
- Goal/primary-secondary-goal tracking for trusty-agents personas (as opposed
  to tcode).
- Making focused-mode-shaped context assembly the default for unfocused turns.

These five are, as far as this search can tell, un-filed. Not determined
whether they exist under wording this search didn't try (issue search is
keyword-based and can miss paraphrases).

---

## Appendix — file:line index of everything cited

- `crates/trusty-agents/src/ctrl/pm_task/dispatch/classification.rs` — 1-70 (module doc), 118-160 (`is_valid_label`/`parse_marker`), 236-244, 263-300 (`build_turn_context`), 305-352 (`assemble_focused_block`, incl. 314-317 stub), 391-461 (`finish_turn`, 448 `tokio::spawn`), 429-436 (persistence gated on `enabled`), 496-501 (`should_refresh_summary`), 506-560 (`maybe_summarize_workstream`, 544 model choice).
- `crates/trusty-agents/src/ctrl/pm_task/dispatch/persona_memory.rs` — 1-67 (module doc), 84 (`RECALL_TOP_K`), 176-183 (`session_id_for`), 548-576 (`acquire_chat_persistence_lock`), 587-663 (`spawn_persist_turn`/`_with_activity`), 689-732 (`persist_activity_turn`/`persist_turn`, RPC names).
- `crates/trusty-agents/src/ctrl/pm_task/dispatch/persona.rs` — 1-88 (module doc, entry point signature).
- `crates/trusty-agents/src/ctrl/pm_task/dispatch/history.rs` — 1-88 (`run_pm_task_with_history`, the separate REPL-history path).
- `crates/trusty-agents/src/ctrl/state.rs` — 25-37 (`ConversationTurn`, in-memory/REPL-owned).
- `crates/trusty-agents/src/api/server/handlers.rs` — 480-522 (GUI dispatch, `&[]` history).
- `crates/trusty-agents/src/context/manager.rs` — 70-94 (`ContextManager`, `budgets` dead code).
- `crates/trusty-agents/src/context/trim.rs` — 1-108 (`trim_to_budget`, eviction/truncation strategy).
- `crates/trusty-agents/src/llm/compress.rs` — 1-58 (`trim_messages_with_manager`).
- `crates/trusty-agents/src/llm/tool_loop/mod.rs` — 239, 251 (live wiring, fixed 0.5).
- `crates/trusty-agents/src/compress/session.rs` — 1-14 (module doc), 79-140 (`SessionCompressor`, dead outside tests/eval).
- `crates/trusty-agents/src/agents/params.rs` — 102-131 (`AgentCompressConfig`), 145-172 (`SessionCompressionConfig`), 178-221 (`WorkstreamContextConfig`).
- `crates/trusty-agents/evals/cto-assistant.toml` — 54-69 (only other `SessionCompressor` reference; a doc-recall eval, not wiring).
- `crates/trusty-agents/ui/src/lib/workstreams.ts` — 1-46 (sidebar data source).
- `docs/specs/trusty-agents-product-spec.md` — 406-566 (§8-9, SPEC-AGENTS-07/08).
- `docs/specs/session-manager-agent.md` — 20-40 (scope note: DOC-14 implements nothing), 96, 658-690, 760-842, 976-1020 (DOC-14 §7, different subsystem).
- `docs/specs/trusty-code-harness-ui.md` — 19-20, 190-258, 379-425, 659-709 (tcode goal slots/compaction, different subsystem).
- `docs/specs/DOC-52-shared-workstream-definition.md` — 106 (Task-vs-workstream-vs-memory-Task-drawer warning), 239-252 (infinite-thread framing, tcode).
- `docs/trusty-agents/spec/PRD.md` / `COMPONENTS.md` — 3-8 (both headed "historical baseline," discounted).
