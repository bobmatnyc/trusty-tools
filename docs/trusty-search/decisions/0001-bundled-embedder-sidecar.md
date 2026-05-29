# 0001. Bundled, supervised `trusty-embedderd` embedder sidecar

- **Status:** Accepted
- **Date:** 2026-05-29
- **Scope:** Crate `trusty-search`
- **Supersedes / Superseded by:** —

## Context

trusty-search embeds code chunks with all-MiniLM-L6-v2 (INT8, 384-dim) via the
ONNX Runtime (and CoreML on Apple Silicon). The ONNX execution-provider arena is
the daemon's **largest and spikiest** memory consumer: a reindex issues hundreds
of `embed_batch` calls, and ORT allocates working memory proportional to batch
size *during* each call, producing RSS spikes the between-batch poller cannot
catch (see [research/bm25-memory](../research/bm25-memory-2026-05-28.md) and
issue #95). Running embedding *in-process* meant the search daemon's address
space inherited that volatility, raising OOM and (on macOS) jetsam-kill risk and
inflating steady-state RSS.

Industry ML-serving practice (Triton, vLLM, TEI, ollama) separates the model
server from its callers. The supervisor module records this rationale directly:
*"This aligns with industry-standard ML serving topology … and reduces
trusty-search daemon RSS substantially by moving the ONNX arena out of the search
process"* (`crates/trusty-search/src/service/embedder_supervisor.rs`). The work
spans issues #110 (split the embedder into a dedicated process), #181 (#110
Phase 2: auto-spawn + manage as a core subprocess), #187 (bundle as a second
binary), #164 (consolidate the embed/BM25 daemons), and #315 (lazy spawn + idle
shutdown). Peak-RSS was measured sidecar-vs-in-process under #282.

A naive sidecar would force operators to install and launch a second binary by
hand — unacceptable for a tool whose product promise is *one install, one
daemon*.

## Decision

We will run embedding in a **separate, supervised subprocess** named
`trusty-embedderd`, and bundle it so a single `cargo install trusty-search`
installs **both** binaries:

- `trusty-embedderd` is a workspace crate (`crates/trusty-embedderd/`) declared
  in trusty-search's manifest **both** as a Cargo dependency **and** as a second
  `[[bin]]` whose shim (`src/bin/trusty-embedderd.rs`) calls
  `trusty_embedderd::run()`. One install command, two binaries, zero logic
  divergence.
- The search daemon owns the sidecar's lifecycle through an
  `EmbedderSupervisor` (`src/service/embedder_supervisor.rs`) that re-exports the
  shared `trusty_common::embedder_client` supervisor types, with startup-timeout,
  restart-backoff, and max-restart limits.
- Spawn is **lazy** (`LazyEmbedderHandle`, #315): binary discovery runs at boot
  and fails fast with an actionable install hint, but the process is started on
  the **first embed request**, and may be idle-shut-down
  (`TRUSTY_EMBEDDERD_IDLE_SHUTDOWN_SECS`) and re-spawned.
- The transport is configurable via `TRUSTY_EMBEDDER`: `auto`/`stdio` (default,
  supervised stdio subprocess), `in-process` (an explicit, never-silent escape
  hatch for tests/constrained hosts), `http://…`, `unix:/…`, or `candle`.
- A priority `embed_pool` (`src/service/embed_pool.rs`, #41) sits in front of the
  sidecar so interactive search embeddings drain before background reindex work.

## Consequences

- **Positive:** the search daemon's address space no longer carries the ONNX
  arena, cutting steady-state RSS and removing the in-process jetsam/OOM-kill
  risk during large reindexes.
- **Positive:** the product promise holds — operators still run one install and
  one `trusty-search start`; the sidecar is invisible unless it fails to install.
- **Positive:** lazy spawn means `lexical_only` deployments (daemonized ripgrep)
  never pay the ONNX init cost at all.
- **Positive:** crash isolation — a wedged embedder is restarted by the
  supervisor (within backoff/restart limits) without taking down search.
- **Negative / trade-off:** an extra IPC hop (stdio/UDS) per embed batch, and a
  second supervised process to reason about; mitigated by request batching
  (`batch_queue.rs`) and the priority pool.
- **Neutral:** the in-process path remains available behind an explicit
  `TRUSTY_EMBEDDER=in-process` for tests and hosts where the sidecar cannot be
  installed; it is never selected silently.
