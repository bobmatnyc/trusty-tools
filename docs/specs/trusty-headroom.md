# DOC-32 — trusty-headroom: Reversible Context-Compression Service

**Status:** Draft
**Subsystem:** trusty-headroom (new workspace crate) — context/token compression for LLM harness sessions
**Owner:** Engineering (trusty-tools)
**Last-updated:** 2026-07-03
**Spec ID:** `SPEC-HEADROOM-01~draft` … `SPEC-HEADROOM-09~draft` (DOC-32)
**Builds on:** trusty-mpm token-optimizer (`crates/trusty-mpm/src/assets/hooks/optimizer.toml`,
the existing lossy PostToolUse `Trim/Summarise/Caveman` policy — DOC-32 supersedes it);
the shared-gateway architecture (all trusty daemons behind trusty-console);
DOC-17 Autonomous Multi-Session Managed Harness Runner.
**Cross-ref:** `crates/trusty-search/` (the reference crate-shape: lib + bins + `src/mcp/`),
`crates/trusty-mpm/src/core/session_launch/settings.rs` (hook wiring),
`crates/trusty-mpm/src/core/bundle.rs` (`OPTIMIZER_TOML`),
`crates/trusty-common/` (shared cache/serde substrate).
**Prior art:** Netflix Headroom (`github.com/chopratejas/headroom`) — MIT-or-similar OSS
context-compression proxy/library/MCP; 60–95% token reduction at benchmarked no-accuracy-loss.
This spec is a **clean-room Rust reimplementation of Headroom's proven ideas**, not a fork.

---

## 1. Motivation

Agent harness sessions burn the majority of their token budget on **machine-generated
tool output** — file reads, `cargo`/`git` output, JSON API responses, RAG chunks, stack
traces, code dumps. This content is highly redundant and rarely needed verbatim once the
model has extracted its meaning, yet it counts against input-token billing on every
subsequent turn it stays in context.

trusty-mpm ships a token-optimizer today (`optimizer.toml`: `level = Off|Trim|Summarise|Caveman`),
but it has three fatal limitations:

1. **Lossy and irreversible.** Once output is trimmed, detail the agent later needs is gone —
   silently. This is the single biggest risk of naive output compression.
2. **Not content-aware.** One `level` per tool name; no structural understanding of JSON vs
   code vs logs.
3. **Not wired into managed sessions** (DOC-32 sibling finding: the managed launcher installs
   no `PostToolUse` hook), so it is inert where it matters most.

Netflix's Headroom project demonstrates the state of the art: **reversible** compression
(Compress-Cache-Retrieve), content-type-specific transforms, and benchmarked *no accuracy
loss* (GSM8K ±0.000, BFCL tool-use 97% at 32% compression). This spec adopts those ideas as a
first-class Rust workspace crate, **`trusty-headroom`**, replacing the lossy optimizer.

## 2. Decision & Non-Goals

**Decision:** Build `trusty-headroom` as a **standalone Rust workspace crate** in the
trusty-* ecosystem shape (mirroring `trusty-search`: a `lib` + thin bins + an `src/mcp/`
server), reimplementing Headroom's architecture in Rust for performance and to avoid a
Python runtime dependency.

**Decision — callable service, not a daemon (Phase 1).** The core transforms (structural
JSON crush, code AST reduction, log/text dedup, and the CCR cache) are **purely algorithmic
with zero model-load latency**, so Phase 1 ships as a **stateless callable service**:
a library, a `trusty-headroom` CLI (stdin→stdout, invoked per PostToolUse), and an on-demand
MCP server. **No long-running daemon.** A warm daemon is introduced **only if/when** an
optional ML text-compressor (§6, Phase 2) is added, because ML weights are the only component
that carries load latency worth amortizing.

**Non-goals (this spec):**
- No LLM-based summarization in Phase 1 (that is lossy and slow; CCR + structural transforms
  deliver most of the win losslessly).
- Not a general HTTP egress proxy in Phase 1 (Headroom's "proxy mode"). trusty-mpm launches
  `claude` over OAuth directly, not through a proxy; the PostToolUse-hook + MCP integration
  fits our harness better. Proxy mode is a deferred Phase 3 option.
- Does not replace trusty-memory (long-term knowledge). CCR's cache is short-TTL,
  session-scoped retrieval, not durable memory.

## 3. Architecture Overview

```
                          ┌──────────────────────────────────────────┐
  tool output ──stdin──►  │  trusty-headroom (callable, no daemon)    │  ──stdout──► compressed
  (PostToolUse hook)      │                                          │              + retrieval markers
                          │   ContentRouter (detect type)            │
                          │      ├─ JsonCrusher    (structural)       │
                          │      ├─ CodeReducer    (tree-sitter AST)  │
                          │      ├─ LogDeduper     (line/pattern)     │
                          │      └─ PassThrough    (already terse)    │
                          │   CCR: hash → cache original → marker     │
                          └───────────────┬──────────────────────────┘
                                          │ SQLite (session-scoped, TTL)
                                          ▼
             MCP tools:  headroom_compress · headroom_retrieve · headroom_stats
             (retrieve reads the cache by hash; the model calls it on demand)
```

## 4. Deployment Modes (SPEC-HEADROOM-01)

Three faces over one core library, matching Headroom's library/proxy/MCP triad but
callable-first:

| Mode | Invocation | Use |
|---|---|---|
| **Library** (`trusty_headroom::compress`) | in-process Rust API | direct callers (trusty-mpm daemon, tests) |
| **CLI** (`trusty-headroom compress` / `retrieve` / `stats`) | stdin→stdout, per-call | PostToolUse hook; zero-daemon, exits after each call |
| **MCP** (`trusty-headroom mcp --stdio`) | on-demand stdio MCP server | the `headroom_retrieve` tool the model calls to recover originals |

**SPEC-HEADROOM-01:** All three modes MUST share one core library and produce byte-identical
compression for identical input+config. The CLI MUST cold-start and return in well under the
PostToolUse hook budget (target < 50 ms p95 for algorithmic transforms on typical tool output).

## 5. CCR — Compress-Cache-Retrieve (SPEC-HEADROOM-02..03)

The reversibility guarantee, ported from Headroom.

**SPEC-HEADROOM-02 (Compress + marker):** When a transform compresses a span, it MUST embed a
retrieval marker carrying `{content_hash, original_size, type}` in the compressed output, e.g.
`⟦headroom:sha256=… size=17765 type=json⟧`. Markers MUST be unambiguous, greppable, and cheap
for the model to reference.

**SPEC-HEADROOM-03 (Cache + retrieve):** The original span MUST be persisted verbatim in a
**session-scoped SQLite cache** keyed by `content_hash`, with a configurable TTL (default: the
session lifetime). The `headroom_retrieve` MCP tool MUST return the exact original for a given
hash in O(1) (~1 ms target). If CCR is disabled by config, compression is permanent and no
cache is written (opt-out for callers that accept lossiness).

**Reversibility is the headline feature** — it converts "risky lossy trim" into "safe, the
model can always get the full data back." This is precisely the failure mode the current
`optimizer.toml` cannot avoid.

## 6. Content-Type Transforms (SPEC-HEADROOM-04..05)

**SPEC-HEADROOM-04 (ContentRouter):** A router MUST classify each input (structural
introspection for JSON, MIME/heuristic detection for files, language detection for code) and
dispatch to the appropriate transform, falling back to `PassThrough` for already-terse or
unknown content. Never expand output; if a transform would not shrink a span, pass it through.

**Phase 1 transforms (algorithmic, no model load):**
- **JsonCrusher** — collapse repeated keys across arrays-of-objects (columnar/schema-once),
  elide null/default fields, dedupe repeated sub-trees. Reversible via CCR.
- **CodeReducer** — tree-sitter AST-aware reduction: strip comments/formatting/whitespace and
  optionally unused branches while preserving semantics; supports the workspace's own languages
  first (Rust) then JS/TS/Go/Python. (trusty-analyze already depends on tree-sitter grammars —
  reuse.)
- **LogDeduper** — collapse repeated log lines / stack frames (`… ×N`), strip ANSI, fold
  timestamps.
- **CacheAligner** — stabilize the *leading* bytes of compressed output so downstream prompt
  prefixes remain byte-stable across turns (protects the Anthropic prompt-cache; DOC-32's
  KV-cache audit confirmed trusty-mpm's own prefix is already stable — this keeps compressed
  tool output from perturbing it).

**Phase 2 transform (optional, model-backed — the ONLY reason to add a daemon):**
- **TextCompressor** — a learned prose compressor (Headroom's `Kompress` analog). Carries
  model-load latency, so it justifies a warm process; ship it as an **optional daemon mode**
  behind a feature flag, not in the Phase 1 callable path.

**SPEC-HEADROOM-05:** Transforms MUST be individually toggleable via config and MUST guarantee
semantic-preserving output for structured types (a crushed JSON must round-trip to an
equivalent value; a reduced AST must parse to an equivalent tree).

## 7. Configuration & Policy (SPEC-HEADROOM-06)

Reuse the existing `optimizer.toml` policy seam (`bundle.rs::OPTIMIZER_TOML`) so the migration
is a superset, not a breaking change:

```toml
[headroom]
enabled = true
ccr = true                 # reversible cache on/off
min_bytes = 512            # never bother compressing tiny outputs
[transforms]
json = true
code = true
logs = true
text = false               # Phase 2 ML compressor, off by default
[tools]                    # per-tool overrides (tool name → on/off/aggressiveness)
# "Read" = "off"
```

**SPEC-HEADROOM-06:** Config MUST be read per invocation (hot-editable, no restart), with a
holdout knob (`holdout = 0.1`) reserving a fraction of calls uncompressed for measurement
(§9). Missing/invalid config MUST fail **open** (pass content through uncompressed) — never
break a session over a compression config error.

## 8. Integration with trusty-mpm (SPEC-HEADROOM-07)

**SPEC-HEADROOM-07:** trusty-mpm's managed-session launcher (`session_launch/settings.rs`)
MUST, when the framework is present, wire:
1. a **`PostToolUse`** hook that pipes tool output through `trusty-headroom compress` (callable,
   no daemon) before it enters context; and
2. the **`trusty-headroom mcp`** server into the session's `.mcp.json` so the model can call
   `headroom_retrieve`.

Both MUST be fail-open and honor the existing hook guards (`CLAUDE_MPM_SUB_AGENT`,
`TRUSTY_MPM_DISABLE_HOOKS`). This closes the "optimizer not wired in managed sessions" gap and
does so with the reversible engine rather than the lossy one. (Note: wiring PostToolUse here is
the sibling of the PreToolUse *enforcement* wiring — both are the same one-line-per-event
launcher change.)

## 9. Measurement & Accuracy Gates (SPEC-HEADROOM-08)

Adopt Headroom's discipline — do not assume compression is free.

**SPEC-HEADROOM-08:** The service MUST record per-call `{bytes_in, bytes_out, tokens_saved_est,
transform, retrieved_later}` to `headroom_stats`. A **holdout** fraction MUST run uncompressed
as a control. CI MUST include an accuracy-regression suite: structured round-trip tests (JSON
value equality, AST equivalence) that MUST pass at 100%, plus a tool-use fixture set asserting
that compression does not change agent decisions. Ship compression ratios and accuracy numbers
in the crate README (mirroring Headroom's published table).

## 10. Licensing & Dependency Analysis (SPEC-HEADROOM-09)

**SPEC-HEADROOM-09:** Before implementation, an owner MUST verify Headroom's license and record
it here. This spec assumes a **clean-room reimplementation of published techniques** (structural
JSON compaction, AST pruning, CCR marker+cache) — algorithms, not copied code — so no license
encumbrance transfers. Do NOT vendor Headroom source or its `Kompress`/Magika model weights
without an explicit license review. Rust deps: prefer `rusqlite` (CCR cache), the tree-sitter
grammars already in-tree (trusty-analyze), `serde_json` (JsonCrusher), `sha2` (hashing) — all
already in `[workspace.dependencies]` or trivially addable.

## 11. Phasing

- **Phase 1 (this spec's core):** callable crate — lib + CLI + MCP; ContentRouter + JsonCrusher
  + CodeReducer + LogDeduper + CacheAligner; CCR over SQLite; `optimizer.toml`-compatible
  config; trusty-mpm PostToolUse + retrieve-MCP wiring; measurement + round-trip CI. No daemon.
- **Phase 2 (optional):** ML `TextCompressor` behind a feature flag + warm daemon mode
  (embedder-supervisor pattern from trusty-search) — only if Phase-1 measurement shows prose is
  the residual token sink.
- **Phase 3 (deferred):** proxy mode; image/binary compression; trusty-console gateway
  integration + dashboard stats.

## 12. Open Questions

1. Cache backing: `rusqlite` vs an embedded KV (sled) — SQLite chosen for queryability + parity
   with Headroom; confirm at implementation.
2. Marker syntax must survive the model faithfully round-tripping it into a `headroom_retrieve`
   call — validate the chosen delimiter isn't mangled by tokenization.
3. Where the CCR cache lives on disk (per-session workspace vs a shared `~/.trusty-tools/headroom`
   with session-scoped keys) — leans shared-with-TTL for cross-tool dedup.
4. Interaction with trusty-mpm's `--append-system-prompt-file` prefix: confirmed cache-stable
   today; ensure compressed tool output never lands in the cached prefix region.
