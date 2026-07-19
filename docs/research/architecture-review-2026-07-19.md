# trusty-tools Architecture Review — 2026-07-19

**Date:** 2026-07-19
**Participants:** Codex external adversarial review (input) · three internal
read-only research passes (crate architecture, documentation architecture,
security & testing architecture) · PM synthesis · Bob's decisions inline
**Status:** Findings accepted by Bob; T0+T1 executing, T2 spec in flight.
**Refs:** #3278 (strategic-concerns tracking)

## Table of Contents

- [Framing (Bob, verbatim intent)](#framing-bob-verbatim-intent)
- [Decisions already made by Bob (2026-07-19)](#decisions-already-made-by-bob-2026-07-19)
- [One-paragraph verdict](#one-paragraph-verdict)
- [Tranche 0 — mechanical fixes (S effort, executing now)](#tranche-0--mechanical-fixes-s-effort-executing-now)
- [Tranche 1 — security architecture (executing now)](#tranche-1--security-architecture-executing-now)
- [Tranche 2 — shared working-model initiative (spec in flight, code awaits Bob)](#tranche-2--shared-working-model-initiative-spec-in-flight-code-awaits-bob)
- [Tranche 3 — documentation system](#tranche-3--documentation-system)
- [Tranche 4 — structure & testing](#tranche-4--structure--testing)
- [Explicitly leave alone](#explicitly-leave-alone)
- [Appendices](#appendices)
- [Appendix A — Crate Architecture & Code-Placement Review](#appendix-a--crate-architecture--code-placement-review)
  - [Crate inventory vs product surfaces](#crate-inventory-vs-product-surfaces)
  - [Findings, ranked by maintenance cost](#findings-ranked-by-maintenance-cost)
    - [Finding 1 — trusty-common is a platform inside the platform (confirms Codex)](#finding-1--trusty-common-is-a-platform-inside-the-platform-confirms-codex)
    - [Finding 2 — the mpm/tcode shared working model is not actually shared (most consequential)](#finding-2--the-mpmtcode-shared-working-model-is-not-actually-shared-most-consequential)
    - [Finding 3 — trusty-agents reaches directly into three other surfaces](#finding-3--trusty-agents-reaches-directly-into-three-other-surfaces)
    - [Finding 4 — the CTO cluster is an undeclared 9th product family](#finding-4--the-cto-cluster-is-an-undeclared-9th-product-family)
    - [Finding 5 — console's generic-proxy shape produced the CSRF gap](#finding-5--consoles-generic-proxy-shape-produced-the-csrf-gap)
    - [Finding 6 — tcode's inference layer sits outside the trusty_common::inference consolidation](#finding-6--tcodes-inference-layer-sits-outside-the-trustycommoninference-consolidation)
  - [Target end-state (text diagram)](#target-end-state-text-diagram)
  - [Leave-alone list](#leave-alone-list)
  - [Codex cross-references](#codex-cross-references)
- [Appendix B — mpm/tcode Documentation-Architecture & Style Review](#appendix-b--mpmtcode-documentation-architecture--style-review)
  - [(a) Instruction-layer map (current)](#a-instruction-layer-map-current)
  - [Proposed canonical layering (target)](#proposed-canonical-layering-target)
  - [Mid-task directive check: is tcode's goal canonized? **No — and text contradicts it**](#mid-task-directive-check-is-tcodes-goal-canonized-no--and-text-contradicts-it)
  - [(b) Ranked doc-architecture problems](#b-ranked-doc-architecture-problems)
  - [(c) Unified doc-convention paragraph (proposed canon, to live in DOC-38 §1)](#c-unified-doc-convention-paragraph-proposed-canon-to-live-in-doc-38-1)
  - [(d) Single-sourcing proposal for mpm/tcode shared assets](#d-single-sourcing-proposal-for-mpmtcode-shared-assets)
  - [(e) Mechanical guards proposed](#e-mechanical-guards-proposed)
  - [Codex cross-references](#codex-cross-references-1)
- [Appendix C — Security-Architecture & Testing-Architecture Review](#appendix-c--security-architecture--testing-architecture-review)
  - [(a) Trust-boundary table](#a-trust-boundary-table)
  - [(b) Ranked systemic security recommendations](#b-ranked-systemic-security-recommendations)
    - [Shared working-model lens (mpm + tcode)](#shared-working-model-lens-mpm--tcode)
  - [(c) Per-surface test coverage](#c-per-surface-test-coverage)
  - [(d) Target CI pipeline](#d-target-ci-pipeline)
  - [(e) Accept-as-is](#e-accept-as-is)
  - [Load-bearing files for follow-up](#load-bearing-files-for-follow-up)

Triggered by Bob after the Codex external adversarial review
(`~/Documents/Codex/2026-07-18/i-w/outputs/trusty-tools-adversarial-review.md`,
strategic concerns tracked in #3278). Scope per Bob: **code architecture review
+ mpm/tcode documentation-style review** — not a feature review. Conducted as
three parallel read-only research passes (crate architecture, documentation
architecture, security & testing architecture), synthesized by the PM.

## Framing (Bob, verbatim intent)

- The product surfaces are: **memory, search, analyze, review, tga, mpm, code,
  agents**. The rest of the ecosystem exists to SUPPORT those eight.
- "I'm generally very happy with the high-level PM + instructions + subagents
  model we've developed with mpm. Tcode should share this with the exception
  that it's not bound to Anthropic's models or the claude-code harness. The
  goal of tcode, if it's not already canonized, is to build the harness around
  the mpm working model and workflow."

## Decisions already made by Bob (2026-07-19)

1. **CTO cluster fate:** cto-assistant / tc-services / trusty-cto-db migrate
   INTO the agents surface as Bob's personal assistant agent;
   trusty-gworkspace becomes a running MCP service. Not a 9th product family,
   not a repo extraction.
2. **Sequencing:** Tranche 0 + Tranche 1 execute immediately; Tranche 2 gets a
   spec (new DOC-N) drafted in parallel for Bob's approval before any T2 code
   moves.

## One-paragraph verdict

The eight product surfaces are real and mostly clean — the crate map matches
them better than expected, and each review produced an explicit leave-alone
list. The three systemic problems: **(1) the mpm working model is not actually
shared with tcode** — the scaffolding exists (`trusty-agents-common`) but the
load-bearing pieces (PM instructions, delegation logic, persona schema,
circuit breaker, trust gating) all live inside trusty-mpm, and tcode is
independently reimplementing them; **(2) security protections are per-crate
accidents, not architecture** — console got a proper origin guard (#3280), but
search/memory/analyze have no guard on destructive endpoints and trusty-agents
defaults to `0.0.0.0` with auth off; **(3) documentation/conventions have no
single source** — the same rule lives in 6+ hand-synced places, and the #2300
double-CLAUDE.md bug was found reproducing live because nothing gates the
regression.

## Tranche 0 — mechanical fixes (S effort, executing now)

| # | Fix | Why |
|---|---|---|
| 1 | CI guard: fail if root `CLAUDE.md` is tracked; repair the stale `.base` checkout (on branch `pr-2236`, 423 commits behind — resurrects #2300 double-context-load in live sessions) | Live regression, 1-line fix |
| 2 | Sync branch protection to all 11 blocking checks (only 6 mechanically required today) | Cheapest gap-close found |
| 3 | Scheduled `cargo audit` workflow — SECURITY.md falsely claims it runs in CI; 3 High advisories open with no re-triage trigger (#2433, #2434, #2437) | Doc/code contradiction on a security claim |
| 4 | tm daemon origin guard: single handler → router-wide | Same #3268 pattern, known template |
| 5 | Sever `trusty-agents-local → cto-assistant` edge; drop trusty-agents' Cargo deps on memory/search (subprocess-MCP integrations don't need the libs) | Pure edge cleanup |
| 6 | Fix false `serde_yaml` safety comment; commit agents-ui `pnpm-lock.yaml`; explicit CSP in mpm-gui | Hygiene |

## Tranche 1 — security architecture (executing now)

7. **Origin guard becomes a shared primitive in trusty-common** (behind
   `axum-server` feature), console migrates to consume it, then applied
   router-wide to search, memory, analyze. Memory is the worst current
   exposure (unguarded palace/drawer DELETE, full JSON-RPC, daemon shutdown —
   all CORS-permissive). (S–M)
8. **trusty-agents: default bind loopback + require token for non-loopback.**
   Qualitatively the worst single gap: unauthenticated LAN-reachable
   subprocess-spawning service by default. (M) *(queued behind #7)*
9. **`docs/reference/threat-model.md`** reconciling ADR-0011's "console is the
   only HTTP surface" with reality (five daemons bind their own listeners). (M)
   *(queued)*

## Tranche 2 — shared working-model initiative (spec in flight, code awaits Bob)

10. **Canonize tcode's Harness Identity** (new DOC-N + vision-spec section);
    current docs actively contradict it ("Claude-Code-native"). Once landed,
    the `unimplemented!()` Bedrock gap becomes a spec violation. (S)
11. **Move the working model into trusty-agents-common:** harness-neutral
    `PM_WORKING_MODEL.md` both prompt assemblers consume (thin harness-binding
    appendices, mirroring BASE_PM's floor-appended-last pattern); delegation
    matrix / workflow phases as shared data; ONE agent-persona schema
    (Markdown+frontmatter vs TOML — open question for Bob); migrate
    `circuit.rs` + `project_trust.rs` out of trusty-mpm (pure state-machine
    code, already isolation-tested — without this tcode ships a weaker safety
    posture silently); harness-conformance test tier following the
    `connectors/test_kit.rs` precedent. (L — its own epic)
12. **tcode's provider/LLM layer folds into `trusty_common::inference`**
    (#2400/#2409/#2410) as an additional consumer — fixes Bedrock at the root. (L)
13. **CTO-assistant migration per Bob's decision** (see above). Phased in the
    T2 spec.

## Tranche 3 — documentation system

14. One canonical convention paragraph (drafted — see Appendix B §c) living in
    DOC-38; the 6+ restatements shrink to pointers.
15. Delete or banner `.claude-mpm/INSTRUCTIONS.md` — confirmed unread by any
    code path, actively stale, costing sync effort per PR (fold still-true
    delivery-chain content into `.trusty-mpm/INSTRUCTIONS.md` first).
16. Mechanical guards with in-repo precedent: skill-asset parity gate (extend
    `check_agent_assets.sh` — agents gated, skills not), doc-catalog integrity
    lint (DOC-N uniqueness; catalog self-admits two gaps), **generated**
    crate-map/README table from `cargo metadata` (#3276-class drift already
    regressed once via #1317).

## Tranche 4 — structure & testing

17. Extract `memory_core` from trusty-common into the memory surface — the one
    surgical cut for the "platform inside the platform"; explicitly NO
    big-bang split of the rest. (M)
18. Console proxy → explicit per-daemon route allowlist — the generic-forward
    shape produced #3268 and will regenerate the gap class. (M)
19. Turn on the frontend tests that already exist — 14 real test files (incl.
    a genuine Playwright spec) across agents/code-gui/search are dark in CI;
    tier-1 blocking vitest for those three, build-only advisory for the rest.
    Closes #3275 at near-zero authoring cost. (S–M)
20. Extend daemon-smoke to memory/analyze; remove `continue-on-error` from
    search's (currently a required check that cannot fail). (S)

## Explicitly leave alone

analyze→review and review→tga feature deps · sidecar daemon crate split
(embedderd, bm25-daemon) · the line-cap ratchet · `#[ignore]` population (116,
disciplined) · secret-redaction / two-gate fleet-secret delivery
(reference-quality) · existing gate set (under-coverage, not over-gating) ·
trusty-mpm's dev-only trusty-review dep · tga short-name publishing.

## Appendices

- **Appendix A** — Crate architecture & code-placement review (full report)
- **Appendix B** — mpm/tcode documentation-architecture & style review (full report)
- **Appendix C** — Security & testing architecture review (full report)

## Appendix A — Crate Architecture & Code-Placement Review

Read-only inspection (worktree `2eb72dca`, HEAD `3bede589`). Lens: mpm and
tcode judged as **two harnesses over one shared working model** per Bob's
directive.

### Crate inventory vs product surfaces

21 crates under `crates/`. `docs/reference/crate-map.md:9` claims "20 members"
and omits `trusty-review`, `trusty-progress`, `trusty-console`,
`cto-assistant`, `tc-services`, `trusty-cto-db`, `trusty-gworkspace` from its
tree — confirms Codex finding #7 (stale docs), tracked in #3276.

| Crate | Classification | Raw .rs lines | Internal deps | Verdict |
|---|---|---|---|---|
| trusty-memory | Product: memory | 38,551 | bm25-daemon, common | Frontend-only; storage engine lives in trusty-common's `memory-core` feature (Finding 1) |
| trusty-search | Product: search | 92,352 | common, embedderd | Clean |
| trusty-analyze | Product: analyze | 28,426 | common, review (optional feature, documented) | Clean, intentional |
| trusty-review | Product: review | 59,898 | tga, common | Clean, intentional (#558) |
| trusty-git-analytics (tga) | Product: tga | 54,716 | common | Clean |
| trusty-mpm | Product: mpm | 161,248 | agents-common, common, progress, review (dev-dep only) | Largest crate; owns the entire working-model asset bundle (Finding 2) |
| trusty-mpm-gui | Support (mpm desktop shell) | 329 + Svelte | none (HTTP client) | Thin, legitimate; CI-excluded |
| trusty-agents (tagent) | Product: agents | 119,051 | agents-common, code, common, memory, search | Most boundary-crossing crate (Finding 3) |
| trusty-agents-common | Support (shared) | 5,115 | none | Best-designed shared crate; under-utilized (Finding 2) |
| trusty-agents-local | AMBIGUOUS | 23 | cto-assistant, agents | Near-empty shim reaching into the CTO cluster (Finding 4) |
| trusty-code (tcode) | Product: code | 15,404 | common ONLY | Does not consume trusty-agents-common despite `harness_doc::tcode()` existing for it (Finding 2) |
| trusty-common | Support (core) | 67,680 | none | Platform inside the platform: 43 modules, ~29 features (Finding 1) |
| trusty-embedderd | Support/infra sidecar | 1,217 | common | Fine |
| trusty-bm25-daemon | Support/infra sidecar | 1,920 | common | Fine |
| trusty-progress | Support/infra | 1,076 | none | Fine — good extraction precedent (#1315) |
| trusty-installer | Infra/tooling | 15,124 | common, progress | Fine |
| trusty-console | Infra (operator dashboard) | 7,941 | common | Generic HTTP gateway over every daemon — the CSRF gap was a symptom of this shape (Finding 5) |
| cto-assistant | AMBIGUOUS (no surface) | 180 | tc-services, agents-common, cto-db | CTO cluster (Finding 4; Bob decided: migrate into agents) |
| tc-services | AMBIGUOUS | 1,159 | cto-db, gworkspace | CTO cluster |
| trusty-cto-db | AMBIGUOUS | 634 | none | CTO cluster |
| trusty-gworkspace | AMBIGUOUS (mini-product) | 3,694 | common | Standalone Google Workspace MCP server, 43 tools (Bob decided: running MCP service) |

### Findings, ranked by maintenance cost

#### Finding 1 — trusty-common is a platform inside the platform (confirms Codex)

43 top-level modules (`bm25`, `catchup`, `chat`, `claude_config`, `embedder`,
`memory_core`, `mcp`, `monitor`, `rpc`, `symgraph`, `tickets`, `update`, …)
behind ~29 feature flags; 67,680 raw lines; the sole runtime dependency all 8
surfaces share. The `memory-core` feature means trusty-memory (the surface) is
a thin MCP frontend while its storage engine lives one crate away
(`docs/reference/former-repos.md:10` documents the split — known, accepted-but-
odd). Cost: any change to a widely-used module requires re-verifying every
surface (admitted overhead in CLAUDE.md's cross-crate workflow).

**Remedy:** no big-bang split — feature discipline already limits binary blast
radius. Extract `memory_core` into trusty-memory (or `trusty-memory-core`) so
the surface owns its storage. Leave mcp/rpc/bm25/session-naming/migrations/
daemon_guard in trusty-common (genuinely cross-surface). Effort **M**.

#### Finding 2 — the mpm/tcode shared working model is not actually shared (most consequential)

Evidence the model IS meant to be shared and the pattern exists once:
`crates/trusty-agents-common/src/harness_doc.rs` — "Canonical
harness-understanding instructions shared between trusty-mpm SM and future
t-code overseer (DOC-21)… must live in one place." Four accessors
(`agnostic()`, `mpm_session_manager()`, `tcode()`, `overseer()`) backed by
`assets/harness_understanding/*.md`.

Evidence the pattern stops there:
- trusty-code depends only on trusty-common — `harness_doc::tcode()` is dead
  code from tcode's perspective.
- The PM orchestration content (PM_INSTRUCTIONS/WORKFLOW/AGENT_DELEGATION/
  BASE_PM + 41 agent personas + 19 skills) lives only in trusty-mpm's assets,
  interpreted as prose by the Claude Code LLM.
- tcode reimplements the same concepts in Rust control flow:
  `trusty-code/src/intent/mod.rs` ("The PM system prompt instructs 'always use
  delegate_to_agent'… Classifying input cheaply…") — a bespoke Rust
  reimplementation of routing that is prose in AGENT_DELEGATION.md;
  `agent_loop/mod.rs` is a from-scratch analogue of the PM/subagent cycle.
- Agent-persona schema duplicated in two unshared representations: mpm's
  Markdown+YAML-frontmatter (`extends: base-research`) vs tcode's TOML
  `AgentConfig { agent, llm, system_prompt, tools, runner }`, whose doc
  comment aspirationally claims Claude-Code compatibility without sharing a
  schema definition.

Consequence: every change to "when does the PM delegate" or the persona shape
must be made twice, in two languages (Markdown-for-LLM vs Rust-for-control-
flow), with no drift check.

**Remedy:** extend trusty-agents-common with the working-model layer — shared
workflow-phase/delegation-matrix data both consume; a single persona schema
with a generation path for the other representation; wire trusty-code to
depend on trusty-agents-common and consume `harness_doc::tcode()`. Effort
**L** — its own initiative. *(→ Tranche 2 spec.)*

#### Finding 3 — trusty-agents reaches directly into three other surfaces

Cargo deps on trusty-memory, trusty-search, trusty-code. Memory/search usage
is subprocess-MCP (plugins spawn `trusty-memory serve` and talk stdio) — the
lib dep is unnecessary; a breaking lib API change forces rebuild/bump anyway.
trusty-code dep is real (agents execute tcode as a backend).

**Remedy:** drop memory/search lib deps in favor of trusty-common's
stdio-MCP-client feature (**S**, → Tranche 0); document the agents→code edge
as intentional one-direction integration (**S**, docs).

#### Finding 4 — the CTO cluster is an undeclared 9th product family

cto-assistant → tc-services → trusty-cto-db + trusty-gworkspace map to none of
the 8 surfaces; trusty-agents-local (23 lines) pulls the whole cluster into
the agents dependency graph. **Bob decided (2026-07-19):** migrate the cluster
into the agents surface as his personal assistant agent; gworkspace becomes a
running MCP service. Edge severing is Tranche 0; migration is specced in
Tranche 2.

#### Finding 5 — console's generic-proxy shape produced the CSRF gap

`trusty-console/src/server.rs` forwards arbitrary methods to every daemon via
`/api/{service}/*` and `/proxy/{daemon}/*` — Codex finding #1's root shape.
The #3280 fix lands the guard, but the design means every new daemon endpoint
is automatically proxy-reachable unless excluded.

**Remedy:** explicit per-daemon allowlist of read vs write routes, reviewed at
the same bar as each surface's own API. Effort **M**. *(→ Tranche 4.)*

#### Finding 6 — tcode's inference layer sits outside the trusty_common::inference consolidation

`trusty-code/src/provider/*` + `src/llm/` implement a bespoke provider
abstraction (Bedrock methods `unimplemented!()` — Codex finding #3). Issues
#2400/#2409/#2410 establish `trusty_common::inference` as the intended single
adapter home; tcode is not a listed consumer despite being the crate with the
most to gain (per the model-agnostic directive).

**Remedy:** add tcode as a consumer of the #2400 migration — fixes the Bedrock
gap at the root. Effort **L**. *(→ Tranche 2.)*

### Target end-state (text diagram)

```
PRODUCT SURFACES (8): memory | search | analyze | review | tga | mpm(+gui) | code | agents
        │ (memory owns its own storage after memory_core extraction)
        ▼
SHARED WORKING-MODEL LAYER (harness-agnostic): trusty-agents-common
  harness_doc + delegation-matrix data + single persona schema + adapters + events
  → consumed by BOTH trusty-mpm (prompt assembly) AND trusty-code (intent/agent_loop)
        ▼
CORE SUPPORT LIBS: trusty-common (mcp/rpc/bm25/migrations/session-naming/
  daemon-guard/inference) | trusty-progress | trusty-embedderd | trusty-bm25-daemon
        ▼
INFRA/TOOLING: trusty-installer | trusty-console (explicit per-surface allowlist)

CTO cluster: migrates INTO agents surface (Bob's decision); gworkspace = running MCP service
```

### Leave-alone list

analyze→review optional feature dep (#630) · review→tga (#558) · mpm→review
dev-dep-only (BACK-gate conformance) · tga short-name publishing (#1128) ·
line-cap ratchet (healthy: 14 allowlisted, 0 violations) · mpm-gui CI
exclusion (tracked #3278) · embedderd/bm25-daemon sidecar split (documented
install rationale) · trusty-agents-common itself (extend, don't restructure).

### Codex cross-references

Confirmed: #7 stale docs (crate-map says 20, actual 21; 7 crates missing from
tree; false Elastic-2.0 claim vs workspace MIT). #1 console CSRF (as symptom
of proxy shape). #3 Bedrock (root cause: outside the inference consolidation).
"trusty-common platform inside platform" (43 modules / ~29 flags / 67,680
lines). "Blurry product boundaries" (extended to agents' direct deps).

Not confirmed: the "documentation-style skill byte-identical in both crates"
example as stated — trusty-mpm's crate assets hold 19 tm-* skills and
trusty-code has no bundled skills tree in `crates/`; the duplication exists at
a different layer (see Appendix B, which located the actual copies). The MCP
static-tool-list drift pattern it illustrated IS confirmed (#3264).

## Appendix B — mpm/tcode Documentation-Architecture & Style Review

Read-only, sourced from `origin/main` (`244bc8bb`). **Live finding surfaced
during investigation:** the literal main checkout is on stale branch
`pr-2236`, 423 commits behind (2 ahead), predating #2300 — both it and the
session worktree still carry a tracked root `CLAUDE.md`, which is why this
very session loaded the ~600-line project instructions TWICE. The #2300 bug,
reproducing live, months after being "fixed" on origin/main. Root cause:
sessions provisioned from stale local branches — a live violation of the
repo's own "source of truth = origin/main:HEAD" rule. Sharpest evidence in the
review that instruction-layer fixes don't propagate reliably.

### (a) Instruction-layer map (current)

| Layer | Path | Read by | Live/enforced? |
|---|---|---|---|
| 1. Framework PM defaults | `crates/trusty-mpm/src/assets/instructions/{PM_INSTRUCTIONS,WORKFLOW,AGENT_DELEGATION,BASE_PM}.md` | `instruction_pipeline.rs` / `resolve_pm_prompt`, `include_str!`, fixed concat order, BASE_PM non-overridable floor | Yes — mechanically assembled, unit-tested |
| 2. Project override | `.trusty-mpm/INSTRUCTIONS.md` (689 lines here — former root CLAUDE.md content) | Same resolver, substitutes when present | Yes (PR #3271 targeted it as surface #1) |
| 3. Legacy parallel checklist | `.claude-mpm/INSTRUCTIONS.md`, `.claude-mpm/AGENT_DELEGATION.md` | **Nothing in crates/trusty-mpm/src reads these as instruction content** (grep-confirmed; `.claude-mpm/` in Rust source = legacy migration paths only). Only inbound ref is a cross-link FROM `.trusty-mpm/INSTRUCTIONS.md` | **No** — hand-maintained, actively stale (unconditional Why/What/Test claim; phantom crate `trusty-symgraph` in its delegation table) |
| 4. Claude Code native memory | Ancestor `CLAUDE.md` files | Claude Code binary, natively, before tm content | Yes — the #2300 mechanism, currently un-defused (see live finding) |
| 5. tm-owned CLAUDE_CONFIG_DIR (DOC-34) | `~/.trusty-tools/trusty-mpm/claude-config/` | Managed sessions | Yes |
| 6. tcode's own assembly | `crates/trusty-code/src/prompt/preamble.rs` + `docs/trusty-code/parity-spec.md` | tcode's prompt assembler | Partially — parity-spec APPROVED but tied to the old "model-comparison harness" framing |

**Same rule stated 3+ times, independently editable:** the Why/What/Test
convention appears in `.trusty-mpm/INSTRUCTIONS.md` (full prose + judgment
rule + ticket-attribution addendum), `.claude-mpm/INSTRUCTIONS.md` (stale
one-liner), `docs/reference/common-pitfalls.md`, the `documentation-style`
skill ×2 (mpm + tcode deployed copies, kept "byte-identical" by hand), and
`references/method-function.md` ×2. PR #3271's critic had to chase the
proportionality amendment across all of these by hand and still needed a
second HIGH-severity fix pass. ≥6 independent edit sites, no lint they moved
together, demonstrated track record of missing one.

**Contradiction risk:** layer 1 (BASE_PM floor) vs layer 2 (project override)
— overlap undetected; layer 3 (unenforced) vs layer 2 (enforced) — #3271
burned review effort syncing a file nothing reads, because no marker
distinguishes live surface from human reading aid.

### Proposed canonical layering (target)

1. **Shared working-model layer** — harness-neutral PM/workflow/delegation/
   doc-convention content, single-sourced, generating both harnesses' bundled
   defaults.
2. **Harness-binding layer** — thin: mpm's Claude-Code launch mechanics (tmux,
   CLAUDE_CONFIG_DIR, hooks); tcode's provider/model dispatch + parity-floor.
3. **Project-override layer** — exactly one file per project
   (`.trusty-mpm/INSTRUCTIONS.md` or tcode equivalent), holding only deltas.
   `.claude-mpm/*` deleted (after folding still-true content) or banner-marked
   "not a live surface."
4. **Native harness memory** — root `CLAUDE.md` minimized to a stub per #2300,
   **enforced** by a CI check (`git ls-files` guard), not just documented.

### Mid-task directive check: is tcode's goal canonized? **No — and text contradicts it**

- `crates/trusty-code/README.md`: "Claude-Code-compatible MPM orchestration,"
  "the Claude-Code-native orchestration entry point," Design Constraints:
  "Claude-Code compatible — reads `.claude/` config exactly as Claude Code
  does."
- `docs/architecture/harnesses.md` (ADR-0004, Accepted): tcode's analogy is
  "Claude Code"; provider independence never mentioned; "Integration with
  Claude Code hooks" listed as owned.
- `docs/trusty-code/vision-and-architecture-spec.md`: "A Claude-Code-compatible
  configuration reader" — ambiguous between format-compat (fine) and
  binary-dependence (not).
- Contrast: model-agnostic dispatch is claimed as **trusty-agents'**
  differentiator (PRD, ARCHITECTURE, ADR-0002) — never tcode's.
- Scattered evidence the goal is real in practice: parity-spec runs the same
  PM scaffold across anthropic/openai/qwen/deepseek/gemma; a literal
  `base_preamble_is_model_agnostic` test; per-agent Bedrock/OpenRouter routing
  in README constraints. But Codex #3 (confirmed): CLI hard-requires
  `OPENROUTER_API_KEY`; Bedrock methods `unimplemented!()` — the promise
  exists in docs/tests, not the runtime path.

**Recommendation:** "Harness Identity" section atop the vision spec stating
Bob's two-part thesis verbatim; correct README/ADR-0004 to "reads the same
`.claude/`-shaped config FORMAT"; promote to a numbered DOC-N (product-identity
decision with binding consequences — the Bedrock gap becomes a spec
violation). *(→ Tranche 2 spec, in flight.)*

### (b) Ranked doc-architecture problems

1. **[Impact L / Effort S] No mechanical guard against re-tracking root
   CLAUDE.md** — #2300 is convention-only and just silently regressed. One
   `git ls-files` CI check. Highest value-per-effort in the review. *(→ T0.)*
2. **[H / M] Skills have no drift gate; agents do.** `check_agent_assets.sh`
   (PR #3041/#2958) parity-checks 29 mpm→tcode agent copies with pinned
   deviations — a solid reusable pattern; skills (4 duplicated files) have no
   equivalent; #3093 shows even the agent script isn't fully wired into
   pre-commit. Extend the pattern to `assets/skills/**`.
3. **[M / S–M] Spec-catalog integrity is manual and self-admittedly broken** —
   DOC-34 assigned but never a catalog row; DOC-28 label collision; "next
   free DOC-N" determined by periodic manual re-scans. A
   `check_doc_catalog.sh` (uniqueness + row presence), `check_sld.sh`-shaped.
4. **[M / M] README/crate-map staleness is a recurrence pattern, not a
   one-off** — #3276 notes #1317 was supposed to fix this and regressed.
   Generate the crate table from `cargo metadata` ("generated block, do not
   hand-edit"), CI-checked.
5. **[M / S] Two directories both claiming to hold PM instructions** —
   `.trusty-mpm/` (live) vs `.claude-mpm/` (vestigial, stale). Delete-after-
   folding or banner. Note `.trusty-mpm/INSTRUCTIONS.md:542` still
   cross-references it as authoritative.
6. **[S / S] SLD-lint "~15 min" cost claim unverified** — `sld-lint.yml` has a
   20-min timeout and cold-cache crate build is the likely culprit; confirm
   via `gh run view` before optimizing (likely rust-cache tuning, not lint
   redesign).
7. **[L / S] `documentation-layout.md` vs DOC-38 scope boundary implicit** —
   435 files under docs/; the two conventions don't conflict but neither
   states the boundary; one cross-linking paragraph in each.

### (c) Unified doc-convention paragraph (proposed canon, to live in DOC-38 §1)

*Documentation in trusty-tools follows one convention, Spec-Linked
Documentation (DOC-38), applied proportionally: every public item states Why
(motivation), What (mechanics), and Test (proof) — in full three-line form for
API entry points, design-heavy code, error contracts, safety/TCC behavior, and
cross-crate surfaces, collapsed to a single-line summary where a competent
reader's first guess would already be right. A fourth, additive, opt-in axis —
Spec References — links an item to the DOC-N spec section that governs it,
using the SLD grammar, and is added only where a real spec genuinely governs
the item — never fabricated to satisfy a checklist. Defensive reasoning, issue
history, and change attribution never live as narrative in the doc comment
itself — they're a one-line pointer (`// See #1234`, `// #1234: <reason>`) to
the issue, PR, or ADR that owns the full story. `scripts/check_sld.sh` and
`scripts/check_test_pointers.sh` are the two mechanical gates that keep this
honest.*

This paragraph **replaces** the five separately-maintained restatements; each
shrinks to a one-line pointer at the canonical home.

### (d) Single-sourcing proposal for mpm/tcode shared assets

- **Extend, don't invent:** `check_agent_assets.sh`'s PARITY/PINNED-DEVIATION
  model is right for content that legitimately differs per harness.
- **Shared source where zero deviation exists:** move `documentation-style`
  (and peers) to `crates/trusty-common/src/assets/skills/…`; both crates
  `include_str!` the same path — eliminates the drift class structurally.
- **Working-model layer:** harness-neutral `PM_WORKING_MODEL.md` consumed by
  both `instruction_pipeline.rs` (mpm) and tcode's assembler, each appending
  its harness-binding section — mirrors BASE_PM's proven floor-appended-last
  pattern. *(→ Tranche 2 spec.)*
- **Guard the boundary once shared:** extend the parity model to the new
  shared paths so divergence requires an explicit reviewed pin.

### (e) Mechanical guards proposed

| Guard | Catches | Cost | Precedent |
|---|---|---|---|
| Root/worktree CLAUDE.md re-track check | #2300 regression (live) | Trivial | `check_line_cap.sh` style |
| Skill-asset parity (mpm↔tcode) | documentation-style-class drift | Low | `check_agent_assets.sh` |
| Doc-catalog integrity | DOC-34/DOC-28-class gaps | Low–Med | `check_sld.sh` |
| Generated crate-map/README table | #3276-class staleness (regressed once) | Med | recommended by Codex too |
| SLD/test-pointer runtime audit | unconfirmed 15-min claim | Trivial to check first | n/a |

Existing gates are proportionate — each traces to a specific incident. The gap
is under-coverage (skills, catalog, README generation), not over-gating.

### Codex cross-references

Confirmed + root-caused: #7 stale docs (missing generation step; #1317
regression proves recurrence). "Documentation volume outrunning its authority
model" (DOC-34/DOC-28 admissions). "Comments overwhelm behavior" — the
ticket-attribution rule (PR #3271 addendum) is the direct fix, already landing.
Extended beyond Codex: the #2300 double-load actively reproducing (Codex
couldn't see live session state). Not assessed: Codex's non-doc findings.

## Appendix C — Security-Architecture & Testing-Architecture Review

Sourced from `origin/main` (`244bc8bb`) via `git show`/`git grep` — the
literal main checkout was found on stale branch `pr-2236` (423 behind, not an
ancestor of origin/main; predates the #3280 CSRF fix and two entire CI gates).
That divergence is itself a process-hygiene finding (see Appendix B).

### (a) Trust-boundary table

| Surface | Default bind | AuthN | CORS | Origin/CSRF guard | Verdict |
|---|---|---|---|---|---|
| trusty-console | 127.0.0.1 (--tailscale widens) | none (locality) | permissive | **Yes** — router-wide post-#3280, bind-aware SelfOrigins | Protected |
| trusty-search | 127.0.0.1 hardcoded | none | Any/Any/Any (shared helper) | **None** | **Gap** — unguarded `POST /admin/stop`, index CRUD, `/reindex`, `/config`, `/upgrade`, `/chat` |
| trusty-memory | 127.0.0.1:7070; `--http` any addr | none | same permissive helper | **None** | **Gap (worst)** — unguarded `/admin/stop`, palace/drawer DELETE, KG writes, `/dream/run`, `POST /rpc` (full tool surface) |
| trusty-analyze | 127.0.0.1 hardcoded | none (outbound key ≠ inbound auth) | same | **None** | **Gap** — `/review`, `/analyze/deep`, `/webhooks/github`, `/facts` CRUD |
| trusty-review | 127.0.0.1:7891 | webhook HMAC only | **no CorsLayer** | none, but no CORS grant limits browser reach | Lower-risk |
| trusty-agents (tagent) | **0.0.0.0 literal default** | **optional** bearer, off by default | allow_origin(Any) | none | **Gap, worse class** — unauth LAN-reachable `/api/task` (arbitrary subprocess spawn), session create/attach/terminate, `/rpc` |
| trusty-mpm daemon | 127.0.0.1; optional --tailscale listener | none generic; /pair/* flow | no CorsLayer | **single-handler only** (`coordinator_chat`); /rpc has loopback ConnectInfo check | Partial gap — session pause/stop/decommission, `claude-config/apply`, `/pair/confirm` unguarded under --tailscale |
| trusty-mpm-gui (Tauri) | n/a (IPC→daemon) | n/a | n/a | `"csp": null`, no capabilities file | Minor gap |
| trusty-agents/ui (Tauri) | n/a (→0.0.0.0 tagent API) | n/a | n/a | `"csp": null` but explicit minimal capabilities/default.json | Minor gap (better) |

**No shared guard primitive exists.** `trusty-common::server::with_standard_middleware`
is the one shared thing — and it's the *source* of exposure (permissive CORS),
not mitigation. Zero hits for csrf/origin_guard in trusty-common. Console and
tm each independently reinvented "is this Origin loopback." Same latent-defect
shape as #3268, except the sibling daemons never had a guard at all.

**No single threat model.** ADR-0011 (Accepted): "HTTP is implemented exactly
once — in trusty-console… the console is the only HTTP surface." Reality:
search/memory/analyze/agents/mpm all bind their own listeners. SECURITY.md
covers only installer trust. No threat-model doc exists.

**Codex cross-check:** #3268/#3269 confirmed and fixed (#3280, verified from
diff). Codex's "gap in an existing protection model, not total absence" is
true only for console — siblings have total absence, which Codex (scoped to
console) did not surface.

### (b) Ranked systemic security recommendations

1. **[S–M] Port the guard router-wide to search/memory/analyze** via a shared
   primitive in trusty-common (the #3280 template + tests exist). *(→ T1, in
   flight.)*
2. **[M] trusty-agents: stop defaulting 0.0.0.0 with auth off** — loopback
   default; require `--api-token` (or loud `--insecure-no-auth`) for
   non-loopback binds. Qualitatively the worst gap. *(→ T1 queue.)*
3. **[S] tm daemon guard single-handler → router-wide.** *(→ T0/T1, in
   flight.)*
4. **[S] SECURITY.md falsely claims cargo-audit runs in CI** — add a scheduled
   audit workflow (nightly-cron pattern per e2e-docker.yml), non-blocking
   initially; 3 open High advisories (#2433 serde_yml/libyml, #2434 legacy
   rustls via aws-smithy, #2437 nltk) have no re-triage trigger. *(→ T0.)*
5. **[M] Finish tracked credential consolidation** (#3248 pick_credentials;
   #2643/#3209 resolve_token) before a fourth reimplementation appears.
6. **[M] One authoritative `docs/reference/threat-model.md`** reconciling
   ADR-0011 vs reality; either retire sibling binds or declare them in-scope
   with guard requirements.
7. **[S] Fix false serde_yaml rationale comment** in root Cargo.toml; schedule
   the migration separately. *(→ T0.)*
8. **[S] mpm-gui explicit CSP + capabilities file** (mirror agents-ui). *(→ T0.)*
9. **[S] Commit agents-ui pnpm-lock.yaml** — only UI of seven without one. *(→ T0.)*
10. **[L] Shared working-model security primitives** (below). *(→ T2 spec.)*

#### Shared working-model lens (mpm + tcode)

`trusty-agents-common` already has the scaffolding (HarnessAdapter trait,
per-harness pane adapters, harness docs incl. HARNESS_TCODE.md). But the
security-relevant working-model machinery is physically inside trusty-mpm
only:

- **Circuit breaker** (`core/circuit.rs`) — pure state-machine, zero I/O,
  isolation-tested (`breaker_trips_and_recovers`, `depth_limit_is_enforced`);
  trivially shareable, but lives in the mpm binary crate; tcode (correctly not
  depending on a peer harness) cannot reuse it.
- **Project/MCP trust gating** (`core/project_trust.rs`) — tcode has zero
  equivalent (grep-confirmed).
- **Permission-mode gating** — `--dangerously-skip-permissions` unconditionally
  injected into every tm-spawned session (`core/model_inject.rs:81`),
  justified as "tm-controlled tmux panes under supervision" — a compensating
  control, not a scoping mechanism. The same literal is separately hardcoded
  in agents-common's claude_code adapter for pane-pattern detection — two
  purposes, two crates, no shared "trust/permission mode" concept. tcode won't
  have this flag to borrow; without a harness-neutral abstraction it will
  reinvent the compensating-control story or skip gating entirely.

**Net:** normal sequencing, not negligence — but if tcode ships without these
three primitives it ships a materially weaker safety posture than mpm,
silently, because no shared contract forces parity. Migrate circuit breaker +
project-trust into trusty-agents-common as a named tracked piece of tcode's
roadmap. *(→ T2 spec.)*

### (c) Per-surface test coverage

| Surface | Unit | Integration | Daemon-smoke | Docker E2E | Frontend in CI | Top structural gap |
|---|---|---|---|---|---|---|
| memory | 247 | 13 | none | Scenario 2 | No | ui/ has zero test infra |
| search | 1027 | ~20 | **Yes (CI job)** | Scenario 1 | No (file exists, never run) | The good template, not yet extended |
| analyze | 410 | 2 (one #[ignore]) | none | Scenario 4 | No | Thinnest integration vs its build-risk |
| review | 1300 | 5 | none | **absent** | n/a | Only daemon-shaped crate with zero E2E |
| tga | 717 | 3 | none | none | n/a | Fits CLI risk profile |
| mpm | 3697 | 38 (incl. e2e/) | Yes, live trusty-search | Scenario 3 | No (gui zero) | Only crate with cross-daemon live smoke |
| code | 722 | 14 (mostly *_e2e) | none | absent | **Partial** — 10 real .test.ts, zero CI wiring | Richest frontend suite, dark in CI |
| agents | 1380 | 3 + shell harness | none | absent | **Partial** — real Playwright spec, never run; CI job stubs the frontend | Same |
| console | 92 | 1 | none | none | No | Thin, fits proxy role |

- **#3275 confirmed accurate**: zero vitest/playwright/pnpm-test in all 9
  workflows; 14 real frontend test files across 3 crates are dead weight to
  CI. The agents-ui job deliberately stubs the frontend (`printf > dist/index.html`).
- **#[ignore] population (116): disciplined** — ONNX, live-daemon, tmux,
  LLM-spend, Bedrock-cred, perf clusters, each documented; not cruft.
- **Test-pointer lint is a real instrument, not theater** — verifies cited
  names exist as fn/mod via scoped git grep, self-tested. Caveat: allowlist
  seeded with **881 grandfathered dangling pointers six days ago** — the
  convention was aspirational for most of history; now actively converging
  (#3282 repaired 10).
- **Harness-conformance gap (per Bob's directive):** circuit breaker and each
  pane adapter are individually unit-tested in isolation; `connectors/test_kit.rs`
  proves the shared-conformance pattern for connectors. **No analogous tier
  asserts the same working-model behavior across harness adapters** — the
  structural gap for tcode's maturation. Recommend a harness-conformance tier
  once tcode's adapter surface stabilizes. *(→ T2 spec.)*

### (d) Target CI pipeline

Current: 9 workflows, all parallel, wall-clock ≈ clippy/test ceiling (~15–25
min typical cache-warm). **Branch protection requires only 6 of ~11
conventionally-blocking checks** — sld-lint, test-pointers,
capabilities-drift, agents-ui clippy enforced socially only. Cheapest concrete
fix: sync protection to all 11. *(→ T0.)*

Additions (all in parallel lanes; critical path unchanged):
- **Daemon-smoke extended**: memory + analyze mirroring search's job; remove
  `continue-on-error` from all three (search's is currently a required check
  that cannot fail — a silent no-op gate).
- **Frontend, tiered**: Tier 1 blocking vitest for the 3 UIs with existing
  tests; Tier 2 advisory build/type-check for the other 4. Closes #3275.
- **Scheduled weekly cargo-audit**, non-blocking → promote once the 3 High
  advisories resolve or are waived.

Estimated PR wall-clock unchanged (~15–25 min typical / 45 worst) — additions
ride parallel lanes.

### (e) Accept-as-is

`--include-ignored` convention · review's smaller surface · tga/tcode absent
from docker-E2E (CLI risk profile) · npm/UI advisory triage rationale (should
be written into SECURITY.md, though) · `[patch.crates-io]` table (pruned,
maintained) · Developer-ID signing architecture (single source-of-truth
binary→identity table; TCC churn is tracked platform behavior) · secret
redaction + two-gate fleet-secret delivery (reference pattern) · the
#[ignore] population.

### Load-bearing files for follow-up

`crates/trusty-console/src/routes/origin_guard.rs` (template) ·
`crates/trusty-search/src/service/server/{admin,mod}.rs` ·
`crates/trusty-memory/src/web/{admin,mod}.rs` ·
`crates/trusty-analyze/src/service/routes.rs` ·
`crates/trusty-agents/src/api/server/{routes,auth}.rs` ·
`crates/trusty-mpm/src/daemon/api/origin_guard.rs` ·
`crates/trusty-common/src/server.rs` ·
`crates/trusty-mpm/src/core/{circuit,project_trust,model_inject}.rs` ·
`crates/trusty-agents-common/src/adapters/*` ·
`.github/workflows/{ci,e2e-docker}.yml` · `docs/adr/0011-*.md`
