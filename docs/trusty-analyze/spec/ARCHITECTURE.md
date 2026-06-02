# trusty-analyze — Architecture

> **Status:** Canonical · Living Document
> **Last reviewed:** 2026-06-01
> **Derived from:** code/docs/tickets audit (drift audit v0.4.1)

This document describes *how trusty-analyze fits together*. Components are framed
**Vision / Current / Gap** and tagged ✅ / 🟡 / 🔵 / ⚪ (see
[README](./README.md#status-legend-used-throughout-this-set)).

---

## 1. System Topology

trusty-analyze is a **sidecar daemon**. It owns no corpus; it fetches chunks
from `trusty-search` over HTTP and computes analysis in-process.

```
┌──────────────────────────┐        HTTP/2 (loopback)        ┌────────────────────────────┐
│   trusty-search (7878)    │   GET /indexes/:id/chunks       │   trusty-analyze (7879)      │
│   - BM25 + vector + KG    │ ◄────────── paged corpus ────── │   core/   analysis engines   │
│   - authoritative corpus  │   GET /indexes                  │   lang/   tree-sitter adapters│
│   - GET /health           │ ◄────────── proxied ──────────  │   service/ axum HTTP API      │
└──────────────────────────┘                                 │   mcp/    stdio + HTTP/SSE    │
        ▲  hard dependency                                    │   embedder/ BoW | neural     │
        │  (startup health check)                             │   redb facts store (owned)   │
        └─────────────────────────────────────────────────── │   /ui embedded dashboard     │
                                                              └────────────────────────────┘
```

### Hard runtime dependency (✅)

**Vision:** the analyzer is meaningless without a corpus to analyze.
**Current:** every corpus-touching command (`serve`, `review`, `review-pr`)
performs a single `GET <search-url>/health` probe before binding a port or
opening redb; failure prints a clear message and exits 1 (`main.rs`). There is
no offline mode.
**Gap:** none — this is intentional (`CLAUDE.md` "Hard Runtime Dependency").

### Dependency direction (✅)

```
trusty-search  ──►  trusty-common  (shared wire types)
trusty-analyze ──►  trusty-common  (shared wire types; features = symgraph, cli-help, axum-server)
trusty-analyze ──HTTP──►  trusty-search  (chunk corpus at runtime)
```

`trusty-common` must never depend on either daemon. Note: the crate-local
`CLAUDE.md` describes `trusty-common` as living *inside* the analyze workspace;
post-consolidation it is a sibling workspace crate (`crates/trusty-common/`).

---

## 2. Single-Crate Module Layout

The crate was collapsed from a nested multi-crate workspace into **one
publishable crate** (`lib.rs` docstring; issue #5). Each former crate is now a
top-level module re-exporting its old public API.

| Module | Responsibility | Key paths |
|---|---|---|
| `core/` | Analysis engines + the trusty-search HTTP client | `core/mod.rs` |
| `lang/` | `LanguageAnalyzer` trait, detection, 14 tree-sitter adapters | `lang/mod.rs`, `lang/adapters/` |
| `embedder/` | BoW + neural embedding backends | `embedder/mod.rs` |
| `mcp/` | MCP server: dispatcher + stdio + HTTP/SSE transports | `mcp/mod.rs` |
| `service/` | axum HTTP daemon + embedded UI (feature-gated) | `service/mod.rs`, `service/ui.rs` |
| `types/` | serde wire-format types shared across the HTTP/MCP boundary | `types/mod.rs` |
| `commands/` | CLI subcommand handlers (daemon lifecycle, service, setup) | `commands/` |

### `core/` submodule map

| Submodule | Role |
|---|---|
| `client.rs` | HTTP/2 client to trusty-search; paged chunk fetch |
| `complexity.rs` | Text-heuristic complexity + `compute_complexity_for` dispatcher |
| `complexity_ts.rs` | tree-sitter AST complexity for Rust / TS / JS |
| `quality.rs` | Corpus aggregation: `QualityReport`, hotspots, smelly chunks |
| `blame.rs` | Temporal-decay scoring over `ChunkBlame` (pure, no `git` shell-out) |
| `concept_cluster.rs` | k-means + BoW embedding helper |
| `facts.rs` | redb `FactStore`, stable `xxh3` fact hash |
| `linker.rs` | Cross-chunk KgNode deduplication |
| `scip.rs` | SCIP protobuf → `KgGraph` |
| `registry.rs` | `AnalyzerRegistry` dispatching chunks to language adapters |
| `tools.rs` / `tool_registry.rs` / `tool_impls/` | External-linter plugin trait, discovery, 10 adapters |
| `refactor.rs` | Rule-engine refactor suggestions |
| `review.rs` | Unified-diff review + index cross-reference |
| `explain.rs` | LLM deep-analysis (`DeepAnalysisReport`) |
| `github.rs` | PR diff fetch, comment post, markdown render, webhook HMAC |
| `ner.rs` | Optional ONNX NER over doc comments |

---

## 3. Analysis Pipeline

```
1. Fetch     GET /indexes/:id/chunks  ─► Vec<CodeChunk>      (core/client.rs)
2. Complexity compute_complexity_for(content, lang)          (core/complexity*.rs)
               ├─ rust/ts/js → tree-sitter AST walk
               └─ else / parse-fail → text heuristic (fallback)
3. Smells     threshold rules (LongFunction/DeepNesting/…)   (types/complexity.rs)
4. Grade      A–F per chunk → QualityReport per index         (core/quality.rs)
5. Graph      registry.analyze(&chunks) → per-lang KgGraph    (core/registry.rs, lang/)
6. Link       merge duplicate nodes across windows            (core/linker.rs)
7. Cluster    k-means over BoW/neural embeddings              (core/concept_cluster.rs)
8. Refactor   complexity+smells → suggestions                 (core/refactor.rs)
9. Review     parse diff, cross-ref corpus → ReviewReport     (core/review.rs)
10. Serve     axum HTTP + MCP (stdio/SSE) → JSON              (service/, mcp/)
```

### LLM provider routing for deep analysis (✅) — issue #530 / #531

`core/explain.rs` exports a `deep_analysis` function that resolves the active
LLM provider at call time:

```
TRUSTY_LLM_MODEL (or --model flag)
  ├─ starts with "bedrock/" → strip prefix → BedrockProvider (AWS Converse)
  │     TRUSTY_AWS_REGION (or AWS_REGION, default "us-east-1")
  │     Full AWS credential chain (env, ~/.aws, IAM roles, SSO)
  └─ anything else (or unset) → OpenRouterProvider
        OPENROUTER_API_KEY required; returns DeepAnalysisError::MissingApiKey if absent
```

`BEDROCK_MODEL_PREFIX = "bedrock/"` and `DEFAULT_MODEL = "openai/gpt-4o-mini"`
are exported constants. A bare `bedrock/` with nothing after the slash silently
falls back to `DEFAULT_MODEL` on the OpenRouter path (OpenRouter path because
the stripped model id is empty, causing the Bedrock branch to early-exit).

The `BedrockProvider` is provided by `trusty_common::chat::BedrockProvider`
(enabled via the `bedrock` feature flag on `trusty-common`). The region
resolution order is: `TRUSTY_AWS_REGION` → `AWS_REGION` → `"us-east-1"`.

**Gap:** none — the routing is fully covered by unit tests
(`bedrock_prefix_routing`, `bedrock_prefix_empty_suffix_falls_back_to_default`).

### AST substrate + fallback (✅)

**Vision:** accurate, line-anchored complexity, not substring counting.
**Current:** `compute_complexity_for` dispatches on language: Rust/TS/JS get a
tree-sitter AST walk (`complexity_ts.rs`) that counts each branching node once
(cyclomatic) and weights by nesting depth (cognitive); all other languages or
any parse failure fall back to the dependency-free text heuristic
(`complexity.rs`). The 14 structural adapters in `lang/adapters/` each own their
grammar walk for graph extraction.
**Gap:** AST-accurate complexity is Rust/TS/JS-only; other languages use the
heuristic for the *complexity number* even though they have full structural
adapters for the *graph*.

### Knowledge-graph schema + linker (✅)

`types/graph.rs` defines a language-neutral `KgGraph`. Because trusty-search
chunks are overlapping ~40-line windows, the same symbol appears in several
chunks; `core/linker.rs::link` collapses duplicates by
`(language, kind, qualified_name)`, keeps the widest line range, rewires edges to
the canonical id, and drops resulting self-loops. SCIP ingest (`core/scip.rs`)
produces the same `KgGraph` shape from precise indexer output, complementing the
heuristic adapters with resolved cross-file references.

---

## 4. Surfaces & Framing

### MCP framing (✅) — stdout reserved

**Vision:** an MCP client gets the same capability surface as a curl user.
**Current:** `mcp/mod.rs` holds the JSON-RPC dispatcher (18 tools). Two
transports share it:
- **stdio** (`mcp/stdio.rs`): line-delimited JSON-RPC over stdin/stdout; one
  object per line; notifications suppressed; parse errors returned with `id=null`.
- **HTTP/SSE** (`mcp/sse.rs`, feature-gated): `POST /mcp` synchronous JSON-RPC +
  `GET /mcp/sse` long-lived stream with a `ready` event and 15s keep-alive pings.

`main.rs` installs `trusty_common::init_tracing(1)`, routing **all logs to
stderr** so stdout carries only JSON-RPC framing. This fixed the #66 corruption
where a `TraceLayer` error wrote to stdout. **Gap:** none.

### HTTP daemon (✅)

`service/mod.rs` builds an axum router (~20 routes) on port 7879 (auto-increments
if busy). Endpoint groups: health/index proxy, complexity/smells/quality,
diagnostics/graph/entities/clusters/ner, SCIP ingest, review/deep/webhook, facts
CRUD, and the embedded UI (`/ui`, rust-embed via `service/ui.rs`). Strict
**parity** with the 18 MCP tools.

### `http-server` feature gate (✅)

**Vision:** library consumers shouldn't pull in axum + tower-http just to use
the dispatcher or CLI types.
**Current (#249):** the `http-server` feature (default-on) gates `dep:axum`,
`dep:tower-http`, `trusty-common/axum-server`, the `service` module, and
`mcp::sse`. The `trusty-analyze` binary lists it as a `required-feature`; stdio
MCP stays unconditional. `--no-default-features` drops the HTTP stack.
**Gap:** none — mirrors the `trusty-common` / `trusty-memory` rule.

### Graceful shutdown (✅) — issue #534 / #535

**Vision:** in-flight analysis requests must not be dropped when the daemon is
upgraded or restarted.
**Current:** `service/mod.rs` attaches `with_graceful_shutdown(trusty_common::shutdown_signal())`
to the axum server. `shutdown_signal()` resolves on SIGTERM or SIGINT; axum
drains active connections before the process exits. Use
`launchctl bootout` (SIGTERM) rather than `launchctl kickstart -k` (SIGKILL)
to get the graceful drain window. The `mcp_bridge` binary reconnects
automatically with exponential backoff when the daemon restarts.
**Gap:** none — parity with trusty-search and trusty-memory (all three adopted
graceful shutdown in the same commit).

### MCP deep_analysis timeout (✅) — issue #528 / #529

**Vision:** the MCP client must not time out before OpenRouter returns.
**Current:** `mcp/mod.rs` uses a dedicated `reqwest::Client` with a
150 s per-request timeout (`DEEP_ANALYSIS_MCP_TIMEOUT_SECS = 150`) for all
MCP→daemon HTTP calls. This is intentionally above the OpenRouter 120 s
ceiling to allow for streaming overhead and network jitter, while still
providing a finite bound. The 5 s connect timeout applies to the loopback
TCP handshake only.
**Gap:** none.

### reqwest timeouts + spawn_blocking (✅) — issue #521

**Vision:** no handler thread should block the async runtime indefinitely.
**Current:** all service handlers that call external services use explicit
per-request (30 s) and connect (5 s) timeouts on their `reqwest::Client`
instances. The `run_diagnostics` handler (external linters) and the neural
embedding path wrap blocking work in `tokio::task::spawn_blocking`, freeing
the async worker threads.
**Gap:** none.

---

## 5. Persistence & State

- **Facts store (✅):** the *only* state the analyzer owns. redb table
  `fact_id(u64) → JSON(FactRecord)`; `fact_id` is a length-prefixed `xxh3` hash
  of the triple — stable across toolchains (replaced `DefaultHasher`, issue #64).
  Path is `--facts-path` (default `trusty-analyze.facts.redb`,
  env `TRUSTY_ANALYZER_FACTS`). #67 fixed a read-lock contention spike in
  `list_facts`.
- **No corpus state (✅):** chunks, blame, and call chains are fetched live from
  trusty-search; the analyzer never opens trusty-search's redb files.
- **PID file (✅):** `start`/`stop`/`status` use a PID file under
  `~/.trusty-analyze/` (`commands/daemon.rs`).
- **Embedding model cache (🟡):** neural embedder loads fastembed from
  `--fastembed-cache` (default `.fastembed_cache`, env `TRUSTY_FASTEMBED_CACHE`);
  load failure is non-fatal and degrades to BoW.

---

## 6. Configuration

| Variable / flag | Default | Purpose |
|---|---|---|
| `--search-url` / `TRUSTY_SEARCH_URL` | `http://127.0.0.1:7878` | trusty-search daemon address |
| `--port` / `TRUSTY_ANALYZER_PORT` | `7879` | Analyzer listen port (auto-increments if busy) |
| `--facts-path` / `TRUSTY_ANALYZER_FACTS` | `trusty-analyze.facts.redb` | redb facts file |
| `--fastembed-cache` / `TRUSTY_FASTEMBED_CACHE` | `.fastembed_cache` | Neural model cache dir |
| `--mcp` | off | Run MCP stdio loop in the `serve` process |
| `--mcp-port` | off | Run MCP HTTP/SSE on a separate port |
| `OPENROUTER_API_KEY` | — | Deep-analysis LLM key (OpenRouter path); returns 400 if absent and a non-Bedrock model is selected |
| `TRUSTY_LLM_MODEL` / `--model` | `openai/gpt-4o-mini` | LLM model for `deep`. Prefix `bedrock/<model-id>` (e.g. `bedrock/us.anthropic.claude-sonnet-4-6`) routes through AWS Bedrock Converse instead of OpenRouter; anything else routes to OpenRouter. A bare `bedrock/` with no trailing model id falls back to the default. |
| `TRUSTY_AWS_REGION` | `us-east-1` | AWS region for Bedrock Converse calls. Takes priority over `AWS_REGION`. |
| `AWS_REGION` | — | Fallback AWS region for Bedrock (overridden by `TRUSTY_AWS_REGION`). |
| `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` / `AWS_SESSION_TOKEN` | — | Standard AWS credential chain for Bedrock. Env vars, `~/.aws/credentials`, IAM roles, and SSO are all supported. No API key is required when a `bedrock/` model is selected. |
| `GITHUB_TOKEN` | — | PR diff fetch + comment post for `review-pr` |
| `ORT_DYLIB_PATH` | — | Path to `libonnxruntime.so` on glibc < 2.38 hosts (requires `--no-default-features --features http-server,load-dynamic`; see #536). |
| `RUST_LOG` | `info` (via `init_tracing(1)`) | Tracing filter (stderr only) |

Cargo features: `default = ["http-server", "bundled-ort"]`; `http-server` (axum
stack); `bundled-ort` (static ORT libs via fastembed, glibc ≥ 2.38);
`load-dynamic` (system `libonnxruntime.so`, glibc < 2.38; requires
`--no-default-features`); `cuda` (CUDA acceleration; requires
`--no-default-features`); `ner` (`dep:ort` + `dep:tokenizers`).

> **Bedrock note (#531):** `trusty-common` must be built with the `bedrock`
> feature (already included in `trusty-analyze`'s dependency declaration:
> `trusty-common = { workspace = true, features = [..., "bedrock"] }`). No
> additional crate flag is needed by callers.

> **ORT backend note (#536 / #538):** prior to v0.4.0 the ORT backend was
> hard-coded to `ort-download-binaries` (bundled static libs). It is now
> feature-selectable: `bundled-ort` (default, glibc ≥ 2.38), `load-dynamic`
> (system ORT, glibc < 2.38 / Amazon Linux 2023), `cuda`
> (load-dynamic + CUDA). The `ner` feature no longer carries its own ORT
> backend; it composes with whichever ORT feature is active.

---

## 7. CLI Command Map

`serve` · `analyze` · `review` · `deep` · `review-pr` · `facts {list,add,delete}`
· `health` · `mcp` · `dashboard`/`dash` · `start` · `stop` · `status`/`st` ·
`doctor` · `completions` · `service {install,uninstall,status,logs}` ·
`setup {claude-code,cursor,claude-mpm,daemon,all}`. *(`main.rs`,
`commands/`)*. Unknown subcommands trigger the shared
`trusty_common::help::suggest` "did you mean?" hint loaded from a bundled
`help.yaml` (issue #216).

---

## 8. Phased Roadmap (design substrate)

The schema already reserves runtime hooks: `KgNodeKind`/`KgEdgeKind` include
`GeneratedFrom` and `RuntimeObservationFor` for Phase 3/4 runtime mapping (🔵),
and the `LanguageAnalyzer` trait in `CLAUDE.md` sketches a
`detect → parse_static → enrich_semantics → prepare_runtime → run_runtime`
lifecycle. The shipping trait (`lang/lang.rs`) currently exposes only the static
path (`analyze_chunks`); the runtime methods are design-only.

---

## 10. Stale-Doc Reconciliation Notes

The audited tree differs materially from the in-crate `CLAUDE.md` / `README.md`:

- **Layout:** single crate (`src/{core,lang,mcp,service,embedder,types,commands}`),
  not the nested `crates/trusty-analyze-*` workspace the CLAUDE.md describes.
- **MCP tools:** 18, not 9.
- **HTTP routes:** ~20 (incl. diagnostics/graph/entities/ner/review/deep/webhook),
  not 8.
- **Adapters:** 14 fully implemented, not "Python/Java/Go stubbed".
- **Cargo features:** `default = ["http-server", "bundled-ort"]` (not just
  `http-server`); `load-dynamic` and `cuda` features added in #536.
- **LLM routing:** deep analysis now supports both OpenRouter and AWS Bedrock
  via the `TRUSTY_LLM_MODEL` `bedrock/` prefix (#531).
- **Version:** v0.4.1 (was v0.1.10 at original spec authoring).

Tracked in [#430](https://github.com/bobmatnyc/trusty-tools/issues/430). This
spec reflects the code, not the stale prose.
