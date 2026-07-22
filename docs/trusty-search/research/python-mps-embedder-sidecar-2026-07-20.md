# Python/MPS embedding sidecar — architecture & validation

**Date:** 2026-07-20 · **Epic:** #3524 (slices 2-4) · **Refs:** #3498, #3493 ·
**Shipped:** trusty-search 0.37.0 + trusty-embedderd-py 0.1.0

Developer/architecture reference for the opt-in Python/MPS embedding sidecar.
The user-facing knobs live in the crate CLAUDE.md ("Embedder Configuration" +
"Python/MPS sidecar tuning"); the crate README
(`crates/trusty-embedderd-py/README.md`) is the code-adjacent entry point. This
document is the deep dive: the wire protocol, the launcher/supervisor seam, the
bootstrap lifecycle, and the cross-platform (Apple Silicon + Linux CUDA) build
notes and validated numbers behind the release.

## Why this exists

On Apple Silicon, a torch/MPS `sentence-transformers` sidecar embeds
**~2.4x faster** than the Rust `ort` (ONNX Runtime) path with numerically
identical results. The spike measured **561 emb/s end-to-end through the real
supervisor** (vs. the 457 emb/s target), fp32 cosine similarity **1.0** against
the ort reference, and fp16 **~1.85x** on CUDA. The sidecar unlocks that
throughput headroom **without** making torch a build- or run-time requirement:
it is **default-off**, opt-in via `TRUSTY_EMBEDDER=python`, and falls back to the
Rust ort embedder on any failure so search never hard-fails.

The design constraint that shaped everything: **zero changes to the existing
supervisor/stdio/protocol wire code.** The Python sidecar impersonates
`trusty-embedderd` exactly, so trusty-search's `EmbedderSupervisor` /
`StdioEmbedderClient` / `LazyEmbedderHandle` drive it unmodified.

## Component map

```
trusty-search  commands/start/embedder.rs   TRUSTY_EMBEDDER=python arm:
                                             eager venv bootstrap + arm LazyEmbedderHandle
      │  (spawns a binary with --stdio, piped stdin/stdout — unchanged)
      ▼
trusty-embedderd-py  (Rust launcher crate — a signable Mach-O)
   src/launcher.rs    sibling/PATH/env discovery + exec into the venv python
   src/bootstrap.rs   uv → venv → uv pip sync → smoke test → .ready sentinel
   src/main.rs        <bin> --stdio: ensure_venv() then exec python -m trusty_embed_sidecar
      │  (exec replaces the process; env inherited across exec)
      ▼
python/trusty_embed_sidecar   (embedded via include_dir!, materialized at bootstrap)
   protocol.py   pure JSON-RPC 2.0 frame parse/build/dispatch (torch-free)
   sidecar.py    reader/worker stdio serve loop + progress-aware shutdown
   model.py      device selection + the real SentenceTransformer encoder
```

## Wire protocol (newline-JSON-RPC over stdio)

The contract is defined by the Rust client
(`trusty-common/src/embedder_client/stdio.rs`) and the reference server
(`trusty-embedderd/src/stdio_server.rs`). The Python sidecar reimplements it
byte-for-byte in `protocol.py`:

- **Framing:** one JSON object per line, newline-terminated. **stdout carries
  ONLY frames**; every log line goes to stderr. stdout is flushed after each
  frame so the client's `read_line` returns promptly.
- **Request:** `{"jsonrpc":"2.0","method":"embed","params":{"texts":[...]},"id":<n>}`
- **Success:** `{"jsonrpc":"2.0","result":{"embeddings":[[..384 f32..],..]},"id":<echo>}`
- **Error:** `{"jsonrpc":"2.0","error":{"code":<i32>,"message":"..."},"id":<echo>}`
- **id echo:** the request `id` is echoed **verbatim**; the client correlates
  responses by id and discards any frame whose id it does not recognise.
- **Readiness:** the model is loaded and given **one MPS warmup encode BEFORE
  the first reply**, so the supervisor's readiness probe only succeeds once the
  sidecar can actually serve.
- **EOF/SIGTERM shutdown:** EOF on stdin (or SIGTERM) stops the reader, drains
  queued work, joins the worker, and exits cleanly (see lifecycle below).
- **Invariants:** empty `texts` → `embeddings: []` (matches the Rust
  short-circuit); unknown method → JSON-RPC `-32601`; any exception inside
  `embed` → `-32603` error frame (one bad request never crashes the loop);
  **all-zero guard** — the Rust HNSW upsert rejects all-zero vectors, so a
  degenerate encode result is nudged to a valid unit vector rather than emitted
  as zeros. Vectors are 384-dim, L2-normalized, matching
  `trusty-common::embedder::EMBED_DIM`.

### Multi-flight concurrency

`StdioEmbedderClient` is **multi-flight**: with `TRUSTY_EMBED_INFLIGHT`
(default 2) it writes several requests before reading their replies. The
sidecar therefore splits work across two threads — a **reader** thread drains
stdin frames into a bounded queue, and a **single worker** thread does the
(GPU-serialized) encode and writes replies. One worker keeps MPS access
serialized (concurrent Metal encode gives no speedup and risks contention)
while still decoupling reading from compute, so a slow or large batch can never
wedge the stdin drain.

## Launcher & the supervisor seam (zero wire changes)

`trusty-search`'s `EmbedderSupervisor` spawns *a binary* with `--stdio` and
piped stdin/stdout. The Python arm reuses that machinery verbatim:

- **Discovery** (`launcher::locate_launcher_binary`) mirrors
  `locate_embedderd_binary`'s search order: (1) `TRUSTY_EMBEDDERD_PY_BIN`
  explicit override; (2) sibling of `current_exe()` (workspace/release build);
  (3) `trusty-embedderd-py` on `PATH`. This is why the launcher must be
  installed **alongside** `trusty-search` (or on PATH) for the opt-in path to
  work.
- **Exec** (`launcher::exec_sidecar`): the launcher process **replaces itself**
  (`exec`) with `<venv>/bin/python -m trusty_embed_sidecar --stdio`, inheriting
  stdio and forwarding args + env (`TRUSTY_EMBED_BATCH_SIZE` et al. survive
  because env is inherited across `exec`). Because it is a real exec, the
  supervisor sees a single long-lived child speaking the wire protocol — there
  is no wrapper process to reap.
- **Failure = clean non-zero exit.** On bootstrap failure the launcher logs to
  stderr and exits non-zero so the supervisor's startup probe fails cleanly.
  trusty-search's `python` arm already fell back to ort at `start` (eager
  bootstrap); this is the belt-and-suspenders path for a lazy respawn onto a
  broken venv.

## Bootstrap lifecycle (uv → venv-in-data-dir → `.ready`)

`bootstrap.rs` materializes the embedded Python project (`include_dir!` of
`python/` — pyproject + hashed `uv.lock` + package + tests) into the
**trusty-search data dir, never the repo**, and builds a pinned venv:

1. **Locate `uv`** (`TRUSTY_UV_BIN` → PATH). Missing `uv` is a bootstrap
   failure → ort fallback.
2. `uv python install` a pinned **CPython 3.11**.
3. `uv venv` at
   `resolve_data_dir("trusty-search")/py-embedder/<lockfile-hash>/venv`
   (honours `TRUSTY_DATA_DIR_OVERRIDE`). The venv dir is **keyed by the
   `uv.lock` content hash**, so a lock change lands in a fresh directory rather
   than mutating a live one.
4. `uv export` narrows the **cross-platform** lock (resolved for BOTH
   macOS-arm64 and linux-x86_64 via `pyproject.toml`
   `tool.uv.environments`) to the *running* platform, emitting a hashed
   requirements file that `uv pip sync` installs (torch 2.9.1 +
   sentence-transformers 5.6.0).
5. **Import+embed smoke test**, then write a **`.ready` sentinel** recording the
   lockfile hash.

Robustness: **disk-space precheck** (~3 GB, torch is large), **bounded
timeout** per step (`TRUSTY_PY_BOOTSTRAP_TIMEOUT_SECS`, default 600) with **one
retry** on transient failure, an advisory **`flock`** over the venv dir against
concurrent bootstraps, and a double-checked `.ready` fast path.

### Two-tier `.ready` re-verification

`.ready` is **not** trusted forever — a post-build corruption (broken native
`.so`, an ABI shift, a half-deleted directory) would otherwise route real
traffic to a broken interpreter. The recheck depth depends on the caller:

- **`ensure_venv()`** — called by the `trusty-embedderd-py` launcher on **every
  respawn** — uses the **cheap, torch-free** `verify_venv_alive` (interpreter
  liveness + an installed-package marker file, bounded ~5s, **no** `import
  sentence_transformers`). A respawn never re-pays torch's import cost, which
  would undercut the point of a longer idle-shutdown window.
- **`ensure_venv_eager()`** — called **once** by the daemon at `start` — uses
  the **full** `verify_full_import_smoke` (a real `import
  sentence_transformers`, bounded ~10s), since that cost is paid only once per
  daemon lifetime.

A venv that fails its recheck is **rebuilt** rather than silently served from a
broken interpreter; if the rebuild fails, the error propagates so the
fall-back-to-ort path fires.

## Idle-shutdown lifecycle

The Python arm gets its own idle-shutdown default via
**`TRUSTY_EMBEDDERD_PY_IDLE_SHUTDOWN_SECS`** (default **1800s / 30 min**),
distinct from the shared `TRUSTY_EMBEDDERD_IDLE_SHUTDOWN_SECS` (300s, tuned for
the lightweight ort sidecar). Rationale: the Python/MPS sidecar's cold restart
is cheap (~2.5–3s: torch import + model load + one MPS warmup) but still worth
avoiding mid-session, so the longer default keeps it warm through a typical work
session while still reclaiming its ~500 MB after genuine extended idle (matters
on the 16 GB minimum-spec tier).

Resolution precedence preserves operator intent: the python var (if set,
including `0`) always wins; else an explicitly-set shared var (any value,
including `0`) is honoured; else the python-specific 1800s default applies. Set
**`0` for always-warm** (idle-shutdown disabled) on higher-RAM machines. This
has **zero impact** on the ort/default arm, which still calls
`SupervisorConfig::from_env()` directly.

### Progress-aware shutdown watchdog

The sidecar's shutdown path does **not** bound the worker-join with a flat
timeout (which would force-exit a legitimately slow-but-healthy drain — e.g. a
reindex burst queued right at SIGTERM — dropping in-flight, not hung, replies).
The worker marks a monotonic `_ProgressTracker` timestamp after each completed
item; `_join_with_progress_watchdog` polls in 1s increments and only
force-exits (`os._exit(1)`) once **no** item has completed for **20s** — a
genuine wedge (e.g. a hung MPS `encode()` mid-batch), not merely a long drain.
The SIGTERM handler raises a dedicated `_ShutdownRequested` exception rather
than touching stdin (the earlier `sys.stdin.close()` inside the handler
reentered a non-reentrant `BufferedReader` and hung the process); per PEP 475
this propagates cleanly out of the interrupted read and `serve()`'s `finally`
still drains and joins.

## Device selection (`TRUSTY_DEVICE`)

`model.py` selects the compute device (`TRUSTY_DEVICE` = `cpu` | `gpu` |
`auto`):

- **`auto`** (default): **MPS** if available (Apple Silicon), else **CUDA**,
  else **CPU**.
- **`gpu`**: MPS if available, else CUDA, else CPU **with a loud warning** — the
  sidecar must not hard-fail (the launcher only falls back to ort on a *non-zero
  exit*, so a working CPU sidecar beats a crash).
- **`cpu`**: force CPU.

**dtype:** fp32 by default (numerically identical to the CPU reference per the
spike). `TRUSTY_PY_EMBED_FP16=1` opts into fp16 on MPS/CUDA (~1.3x faster on
MPS, ~1.85x on CUDA; cosine still ≥ 0.9999). The model is pinned to a specific
HuggingFace revision (`all-MiniLM-L6-v2`) so a bootstrapped venv always
downloads bit-identical weights — the model-weights analogue of the pinned
`uv.lock`. `TOKENIZERS_PARALLELISM=false` is set (via `setdefault`) before any
`transformers` import to avoid an orphan-prone `resource_tracker` child the
single-worker sidecar never benefits from.

## Platform build notes

### Apple Silicon (MPS) — the primary target

Nothing extra: `uv` on PATH (or `TRUSTY_UV_BIN`) and ~3 GB free disk on first
use. `TRUSTY_EMBEDDER=python trusty-search start` bootstraps the venv and, on
`TRUSTY_DEVICE=auto`, uses MPS.

### Linux CUDA (validated on AWS g4dn)

The cross-platform `uv.lock` resolves torch for linux-x86_64, but a CUDA host
needs these to build and run cleanly (validated on a g4dn instance, Ubuntu
22.04, 16 GB GPU):

- **`CC=gcc-12`** for the venv build on **Ubuntu 22.04** — the default toolchain
  version fails the native build; gcc-12 succeeds.
- **Use Microsoft's dynamic ONNX Runtime GPU release**, not pyke's static
  `cu12` tarball, for the CUDA execution provider. (Relevant when the *ort*
  reference path is also exercised on the box for the correctness comparison;
  the pyke static tarball does not link cleanly against the g4dn CUDA stack.)
- **`TRUSTY_SKIP_RAM_CHECK=1`** on 16 GB GPU boxes — the disk/RAM precheck is
  tuned conservatively; a 16 GB GPU box is fine but trips the guard, so skip it
  explicitly.

### Validated numbers

| Metric | Result |
|--------|--------|
| End-to-end throughput (real supervisor, Apple Silicon) | **561 emb/s** (target 457) |
| MPS throughput headroom vs. Rust ort | **~2.4x** |
| fp32 correctness vs. ort reference | cosine **1.0** |
| fp16 speedup (CUDA) | **~1.85x**, cosine ≥ 0.9999 |
| fp16 speedup (MPS) | ~1.3x, cosine ≥ 0.9999 |
| Cold restart (torch import + model load + one MPS warmup) | ~2.5–3s |
| Idle RSS reclaimed on shutdown | ~500 MB |

## Testing

- **Rust unit** (no torch/venv): `cargo test -p trusty-embedderd-py` —
  `bootstrap_tests.rs` covers hash stability, layout derivation, both `.ready`
  fast paths, and disk/uv-missing error surfaces.
- **Python protocol conformance** (torch-free): `cd python && PYTHONPATH=.
  python -m pytest tests/` — id-echo, out-of-order tolerance, empty batch, error
  framing, real-signal SIGTERM shutdown, and the progress-aware watchdog.
- **Real-model correctness gate** (needs torch + model): `TRUSTY_RUN_REAL_MODEL=1
  ... pytest -m real_model` — ≥0.999 cosine vs. reference.
- **Full real-venv e2e through the real supervisor** (`#[ignore]`d):
  `TRUSTY_RUN_PY_E2E=1 cargo test -p trusty-embedderd-py --test e2e -- --ignored`.

## Follow-ups (slices 5-7)

Vendored / download-with-SHA256 `uv` acquisition, `doctor` polish, packaging +
**default-on-Apple-Silicon**, and the bench / #24 soak.

## Spec References

- [`ARCHITECTURE.md`](../spec/ARCHITECTURE.md) — embedder sidecar in the overall
  trusty-search process model.
- [`cuda-embedder-0236-regression-2026-06-05.md`](cuda-embedder-0236-regression-2026-06-05.md)
  — prior CUDA embedder validation context.
- [`candle-metal-validation-2026-05-22.md`](candle-metal-validation-2026-05-22.md)
  — the Candle/Metal vs. ort baseline this sidecar is measured against.
