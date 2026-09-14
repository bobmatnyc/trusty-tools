# Agent class certification by tool-calling capability — research and proposal

**Status:** Informative (research, not a behavior contract)
**Owner:** Engineering (trusty-code, trusty-agents-common)
**Last-updated:** 2026-09-14
**Related:** #7944, #7941, #2892, #7735, DOC-57, DOC-65, ADR-0059

---

## Part 1 — what exists in trusty-tools today

**Three unrelated "tier" concepts, none is a certified capability class.**

| Concept | Location | What it actually gates |
|---|---|---|
| `ModelTier` (Haiku/Sonnet/Opus) | `crates/trusty-mpm/src/core/agent.rs:27-53` | Cosmetic — `from_model_id` does a **substring match** on the model id string (`"opus"`/`"haiku"`/else `Sonnet`) for dashboard colour-coding. Not a capability test: `mystery-model` certifies as `Sonnet` by default (`crates/trusty-mpm/src/core/agent.rs:451`). |
| `ModelTiers` (`lightweight`/`standard`/`high`/`intensive`) | `crates/trusty-mpm/src/core/manifest/schema.rs:499-512`, resolved by `tier_to_model` at `crates/trusty-agents-common/src/agents/builder.rs:352-359` | Manifest-declared aliasing only — `intensive→opus`, `lightweight→haiku`, everything else→`sonnet`. No test suite backs the mapping. |
| `TierAliases`/`expand_model_alias` | `crates/trusty-mpm/src/core/config.rs:53-60,637-660`; `normalize_model_alias` at `crates/trusty-code/src/provider/routing.rs:120-127` | Pure string aliasing (`"opus"→"anthropic/claude-opus-4.5"`, etc.), configurable per-deployment. Same non-verification gap. |
| `ServiceTier` / `ToolExecutor::restricted_tiers` | `crates/trusty-code/src/tools/traits.rs:215-235`; implementors e.g. `crates/trusty-code/src/tools/bash/mod.rs:135-137`, `crates/trusty-agents/src/tools/l0_exec.rs:264-266`, `crates/trusty-agents/src/tools/pm_bridge.rs:210-213,403-405`, `crates/trusty-agents-common/src/lib.rs:246-248` | A **different axis entirely** — RBAC (which tools a caller may invoke: `ReadOnly`, `Analytics`, …), not model capability. Do not conflate with `ModelTier`. |
| `ProviderCapabilities` | `crates/trusty-common/src/inference/registry/mod.rs:209-260`, seed table below it | Per-**provider** (not per-model) static flags: `native_tool_calling`, `tool_dialect`, `streaming`, `prompt_caching`, `structured_output`, `vision`, `detailed_usage_accounting`, `max_context_window`, `default_model`, `credential_env`. Closest existing thing to a capability registry, but it is a hand-seeded best-effort table (doc comment: "concrete adapters in #2403 refine any that drift") — never exercised against a live model, and it says nothing about parallel/multi-turn tool-chain competence. |
| Agent frontmatter `model:` | 42 bundled agents, dumped in full: every one is `model: sonnet` **except** `documentation.md` and `memory-manager.md` (`model: haiku`) — `crates/trusty-agents-common/src/assets/agents/*.md`, `crates/trusty-code/src/assets/agents/*.md`. **No agent in the bundled catalog declares `model: opus`.** Rationale is prose, not measurement: `docs/specs/DOC-65-universal-framework-agents.md` §3.1 assigns tiers by hand-written judgment ("investigation requires synthesizing multiple files… above haiku's single-pass ceiling, below the adversarial-judgment need that would justify opus"). |
| Permissions/autonomy | `docs/specs/agent-config-five-sections.md` (DOC-57) §7, lines 85, 873-952 | Introduces a first-class `[permissions]` section carrying "scopes, tiers, authority flags and autonomy posture" — the closest spec-level precedent for a declared `class:`-like field, but scoped to authorization, not tool-calling competence. |

**The bakeoff module is a strong, reusable certification harness skeleton — but it certifies a *build*, not a *model*.**
`crates/trusty-code/src/bakeoff/` (`mod.rs`, `metadata.rs`, `preflight.rs`, `compare.rs`) and `crates/trusty-code/src/cli/bakeoff.rs` implement `tcode bakeoff-gate`, exercised end-to-end in `crates/trusty-code/tests/bakeoff_gate_e2e.rs`. Full spec: `docs/reference/bakeoff-exit-gate.md`. Mechanics:
- Retained-evidence bundle, one directory per level (`L1/`, `L2/`, `L3/`), each carrying `metadata.json`, `tcode_report.json`, `prompt.txt`, `stderr.log`, `solution.diff`, `verifier.json` — all six required, non-empty (`docs/reference/bakeoff-exit-gate.md:35-49`).
- `metadata.json` already records `invocation.model`, `invocation.provider`, `build.commit`/`binary_sha256`, `source_digests` (instructions/agents/skills hashes) — i.e. it already answers "which model, which harness build, which prompt set" (`docs/reference/bakeoff-exit-gate.md:52-78`).
- `preflight()` rejects incomplete coverage, missing provenance, mock evidence, a stale/dirty runner, or a build mismatch; `compare()` diffs a candidate bundle against a prior accepted baseline and blocks correctness regressions while requiring a written disposition for cost/turn/duration drift (`crates/trusty-code/src/bakeoff/mod.rs` module doc, lines 1-31).
- Today's L1/L2/L3 are **milestone-freeze levels of one coding harness** ("candidate frozen"), run once per milestone, not per-model tool-calling capability tiers. Nothing here is keyed by class or reused across models.

This is exactly the retained-evidence + preflight/compare machinery a class certification suite needs; it is not currently pointed at that job.

**No ADR or spec owns model selection/cost tiers/agent capability as a first-class concept.** `docs/adr/INDEX.md` has ADR-0025 (collapse agent/skill tier *deployment* hierarchies — sourcing, not capability), ADR-0059 (canonical agent standard, one authored source + generated adapters), and DOC-57/DOC-65 as above. No entry addresses model-capability certification.

## Part 2 — industry standards

| Standard | What it separates | Source |
|---|---|---|
| **BFCL v3/v4** (Berkeley Function-Calling Leaderboard, Gorilla, ICML 2025 for v4) | Simple (1 fn, AST match) / Multiple (pick 1 of N) / Parallel (N calls, 1 turn) / Parallel-Multiple / Multi-Turn (stateful tool tracking) / Live (real APIs) / Relevance-detection (must *refuse* when no tool fits — this is the abstention/irrelevance dimension). v4 adds agentic web search, agent memory management, and format sensitivity (schema-rewording robustness). | [gorilla.cs.berkeley.edu/leaderboard.html](https://gorilla.cs.berkeley.edu/leaderboard.html) |
| **τ-bench / τ²-bench** (Sierra) | Multi-turn, policy-compliant tool use against a live simulated backend; τ² adds **dual control** — both agent and simulated user can call tools — across Retail/Airline/Telecom domains. Closest published analogue to "nested delegation/orchestration under a live counterpart." | [github.com/sierra-research/tau2-bench](https://github.com/sierra-research/tau2-bench) |
| **ToolBench / Nexus / AgentBench** | ToolBench: 16k+ real APIs, tool selection/orchestration at scale. AgentBench: 8 environments (OS, DB, web, etc.) for general agent capability, not just calling syntax. Nexus not independently confirmed in this pass — UNVERIFIED. | [github.com/philschmid/ai-agent-benchmark-compendium](https://github.com/philschmid/ai-agent-benchmark-compendium) |
| **SWE-bench Verified** | 500 human-screened real GitHub issues; measures end-to-end code-fix competence, i.e. the *implementer* role specifically, not raw tool-calling. | cited via compendium above |
| **LiteLLM model metadata** | `supports_function_calling(model)`, `supports_parallel_function_calling(model)` — boolean per-model flags, the exact shape `ProviderCapabilities` half-implements today but per-provider instead of per-model. | [docs.litellm.ai/docs/completion/function_call](https://docs.litellm.ai/docs/completion/function_call) |
| **OpenRouter `supported_parameters`** | Per-model array (`tools`, `tool_choice`, `structured_outputs`, `response_format`, `reasoning`, …) returned by its parameters API — a live, queryable per-model capability flag set, closer to what a `class:` resolver needs than a hand-seeded table. | [openrouter.ai/docs/api/api-reference/parameters/get-parameters](https://openrouter.ai/docs/api/api-reference/parameters/get-parameters) |
| **Levels of Autonomy for AI Agents** (arXiv 2506.12469) | L1 Operator → L5 Observer, defined by *user role*, deliberately decoupled from model capability — proposes third-party "autonomy certificates." Directly analogous to the certification-cache idea below, but for autonomy, not tool-calling competence. | [arxiv.org/pdf/2506.12469](https://arxiv.org/pdf/2506.12469) |

**Vendor families, Sept 2026.** Per this workspace's own live session banner (`.trusty-mpm/scrollback.txt:2,849`: `"Fable 5.1 · Claude Max"`), Anthropic's current flagship is branded **Fable** (Opus's apparent successor line) — confirmed live, not a rumor. OpenAI's current flagship variant is reported in the press as **Sol**, part of a GPT-5.6 family (workhorse tier; siblings Terra/mid, Luna/budget) — [forbes.com/.../ai-model-wars-anthropic-extends-fable-access-again-after-openais-sol-release](https://www.forbes.com/sites/tylerroush/2026/07/13/ai-model-wars-anthropic-extends-fable-access-again-after-openais-sol-release/), [techcrunch.com/2026/07/09/openai-launches-its-new-family-of-models-with-gpt-5-6](https://techcrunch.com/2026/07/09/openai-launches-its-new-family-of-models-with-gpt-5-6/). **This "Sol" claim is sourced-to-press only** — neither article was cross-checked against an OpenAI vendor page, so treat it as reported, not confirmed. "sol" as a class label is otherwise consistent with this session's own banner naming a top-tier Anthropic family (Fable), which corroborates that the owner's three top-tier names denote real, current flagship-class models rather than a placeholder. This session itself runs as `claude-sonnet-5` per its own system context, consistent with Sonnet remaining Anthropic's mid tier under the Fable-topped family. Gemini-family tiering and any additional open-model tiering were not independently confirmed in this pass — UNVERIFIED.

## Part 3 — proposed class ladder

Ladder names match the owner's stipulation; certified **by capability, not by model name**.

| Class | One-line definition | Required tool-calling capabilities (BFCL/τ²-bench terms) |
|---|---|---|
| **classifier** | Deterministic text classification/summarization; may name a tool but never chains one | Relevance/irrelevance detection only (must abstain correctly); no live call required to pass |
| **executor** | Single, well-schema'd tool call per turn, no cross-turn state | BFCL Simple + Multiple; schema adherence under a short, fixed context; no parallel or multi-turn requirement |
| **implementer** | Sustained multi-file/multi-step work: parallel calls, multi-turn state tracking, recovers from a failed call without giving up the turn | BFCL Parallel + Parallel-Multiple + Multi-Turn; schema adherence under long context (accumulated diffs/logs); one retry-on-error path exercised; structured output (JSON/diff) validated against schema |
| **orchestrator** | Delegates to other agents/models, holds a multi-agent plan across turns, must correctly abstain from routing when no delegate fits | Everything in implementer, plus τ²-bench-style dual-control (agent *and* a delegate both call tools); nested delegation (a tool call that itself dispatches another certified agent); BFCL Live-equivalent (real MCP tools, not fixtures) |

| Agent role (from `docs/specs/DOC-65-universal-framework-agents.md` §3.1) | Class | Basis |
|---|---|---|
| PM / orchestrator | orchestrator | Delegation is the job description; today's roster has no bundled agent at this tier — Fable/Opus/Sol territory |
| engineer, `*-engineer` variants | implementer | Multi-file sustained reasoning; matches current `sonnet` default |
| qa / web-qa / api-qa, code-analyzer, code-critic, security, local-ops, version-control | implementer | All currently sonnet; adversarial/verification judgment maps to implementer, not orchestrator |
| research | implementer | Synthesis across files; DOC-65 explicitly places it "above haiku's ceiling, below opus" |
| ticketing | implementer | Workflow-state judgment per DOC-65 §3.1 |
| documentation | executor | Currently haiku; formulaic, pattern-following — DOC-65: "no adversarial judgment call" |
| memory-manager | executor | Currently haiku; bounded, schema'd writes |
| classifier tasks (intent routing, triage labels) | classifier | No bundled agent named explicitly today — closest is any haiku-tier sub-step inside `ticketing`/`local-ops` |

## Certification protocol

**Per-class suite.** Reuse the bakeoff evidence-bundle shape (`docs/reference/bakeoff-exit-gate.md`) rather than invent a new one: a `model-cert/` bundle with one directory per BFCL-style category (`simple/`, `multiple/`, `parallel/`, `multi_turn/`, `relevance/`, and for orchestrator `dual_control/`, `nested_delegation/`), each carrying the same six retained-evidence files (`metadata.json`, `report.json`, `prompt.txt`, `stderr.log`, `solution.diff`-or-`tool_calls.json`, `verifier.json`) that `preflight()` already knows how to validate. Extend `crates/trusty-code/src/bakeoff/preflight.rs` with a `class-cert` bundle kind (own category set, own pass thresholds) alongside the existing L1-L3 kind, and give `tcode bakeoff-gate` a `--suite class-cert` flag; `compare()` is reused unmodified to gate re-certification against the model's prior cert as its own baseline.

**Pass thresholds** (per category, no partial credit across categories — a class requires all its required categories to clear):

| Class | Categories required | Suggested threshold |
|---|---|---|
| classifier | relevance | ≥95% correct abstain/act decision |
| executor | simple, multiple | ≥90% AST/schema match, 0 malformed calls |
| implementer | simple, multiple, parallel, parallel-multiple, multi-turn | ≥85% each category, ≥1 demonstrated recovery from an injected tool-error |
| orchestrator | implementer set + dual-control, nested-delegation, live-MCP | ≥85% each, plus a clean nested-delegation trace (delegate certified at implementer or below, no credential leakage per ADR-0026) |

**Cache.** One JSON file per (provider, model, suite version) under `~/.trusty-tools/trusty-mpm/model-certs/<provider>__<model-slug>.json`, alongside this workspace's other `~/.trusty-tools/` state (deployment manifests already live under this root; `.trusty-code/`/`.claude/`/`.codex/` are ADR-0059's *generated adapter* trees and are the wrong place for a cross-host cert cache). Record: `model_id`, `provider`, `class_certified`, per-category scores, `suite_version`, `certified_at`, `harness_commit` (the `tcode` binary hash the run used — same field bakeoff already tracks at `build.binary_sha256`), and `source_digests` for whatever prompt/tool-schema set produced the score, mirroring `docs/reference/bakeoff-exit-gate.md`'s existing `metadata.json` shape.

**Re-certification triggers:** model id changes (new date-stamped snapshot); suite version bump; 90-day staleness; provider-reported capability change (e.g. `ProviderCapabilities`/OpenRouter `supported_parameters` diverges from the cert's recorded flags).

**Roster resolution.** Agent frontmatter gains `class: orchestrator|implementer|executor|classifier` as an alternative to today's `model: sonnet`. Resolution: read all certs for that class from the cache, filter to ones not stale, sort by the existing `crates/trusty-common/src/inference/registry/pricing` module's per-model pricing, pick cheapest certified. An explicit `model:` field, when present, still wins outright (back-compat with all 42 existing bundled agents, none of which need to change on day one). A `class:` with zero certified models for it is a hard error at manifest-load time, mirroring the framework-manifest partition check already enforced by `parse_framework_manifest` (`docs/specs/DOC-65-universal-framework-agents.md` §1).

**Cost per run** (rough, per model): executor ~40 calls / ~15k tokens; implementer ~150 calls (parallel+multi-turn adds turns) / ~120k tokens; orchestrator ~250 calls (dual-control doubles the transcript) / ~300k tokens; classifier ~30 calls / ~8k tokens. These are suite-authoring estimates, not measured — no such suite exists yet in this codebase.

## Improvement recommendations

1. **Symptom:** `ModelTier::from_model_id` certifies unknown model ids as `Sonnet` by default (`crates/trusty-mpm/src/core/agent.rs:44-53`). **Cause:** substring matching with no verification step. **Change:** once a cert cache exists, gate this fallback — an uncertified model id should resolve to no class rather than a silent mid-tier default. **Evidence:** `tier_parses_from_model_id` test (`crates/trusty-mpm/src/core/agent.rs:439-451`) documents the fallback as intended behavior today, which is the gap this proposal closes.
2. **Symptom:** `ProviderCapabilities` is provider-scoped and hand-seeded (`crates/trusty-common/src/inference/registry/mod.rs:209-260`), so it cannot answer "does *this model* pass parallel/multi-turn tool calling," only "does this *provider's dialect* support the wire format." **Cause:** the registry was scoped to wire-protocol differences (#2402), not model competence. **Change:** the class-cert cache proposed above is a natural sibling module in `trusty-common`, not a bolt-on elsewhere — file the suite design as a `trusty-common` epic alongside #2402/#2403 rather than in `trusty-mpm` or `trusty-code` alone, since both already consume `ProviderCapabilities`.

## Prompt feedback

The ask correctly separated "what exists" from "what to propose," which kept the research bounded. One ambiguity: it wasn't specified whether the certification suite should reuse the bakeoff module's existing bundle format or be freestanding — I assumed reuse was preferred given the owner's own framing of "how the bakeoff module could run it," and said so explicitly rather than presenting both as open.
