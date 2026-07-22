# trusty-embedderd-py

Opt-in **Python/MPS embedding sidecar launcher** for
[trusty-search](../trusty-search) (epic #3524, slices 2-4). **DEFAULT-OFF** —
only active when `TRUSTY_EMBEDDER=python`.

On Apple Silicon a torch/MPS `sentence-transformers` sidecar embeds ~2.4x
faster than the Rust ort path with numerically identical results. This crate
is the launcher that bootstraps a pinned Python venv (from a committed, hashed
`uv.lock`) and `exec`s the sidecar, which speaks the EXACT stdio JSON-RPC 2.0
wire protocol of `trusty-embedderd` — so the trusty-search `EmbedderSupervisor`
/ `StdioEmbedderClient` drive it with ZERO wire-code changes.

## Layout

```
trusty-embedderd-py/
├── src/
│   ├── main.rs        launcher binary: ensure venv → exec python -m trusty_embed_sidecar --stdio
│   ├── lib.rs         public surface consumed by trusty-search
│   ├── bootstrap.rs   slice 4 — uv/venv bootstrap (flock, .ready, disk precheck, retry)
│   └── launcher.rs    slice 3 — sibling/PATH discovery + exec
├── python/            slice 2 — the sidecar, embedded via include_dir!
│   ├── pyproject.toml
│   ├── uv.lock        hashed, cross-platform (macOS-arm64 + linux-x86_64)
│   ├── trusty_embed_sidecar/   protocol.py · sidecar.py · model.py · __main__.py
│   └── tests/         pytest conformance (torch-free) + gated real-model gate
└── tests/e2e.rs       #[ignore] real-venv e2e through the real supervisor
```

## Usage

```bash
# Opt in (Apple Silicon). Eager-bootstraps the venv at start; falls back to the
# Rust ort embedder on any bootstrap failure so search never hard-fails.
TRUSTY_EMBEDDER=python trusty-search start
```

Requires `uv` on `PATH` (or `TRUSTY_UV_BIN`) and ~3 GB free disk for the
one-time torch + sentence-transformers download. The launcher binary must be
installed **alongside** `trusty-search` (a sibling of `current_exe()`) or on
`PATH` — that is how `EmbedderSupervisor` discovers it. Override with
`TRUSTY_EMBEDDERD_PY_BIN`.

## How it works

1. **Bootstrap** (`bootstrap.rs`): materializes the embedded `python/` project
   (via `include_dir!`), locates `uv`, installs a pinned CPython 3.11, creates a
   venv in the trusty-search **data dir** keyed by the `uv.lock` content hash,
   `uv pip sync`s a hashed requirements file, runs an import+embed smoke test,
   and writes a `.ready` sentinel. Disk precheck + bounded timeout + one retry +
   `flock` against concurrent bootstraps. A two-tier `.ready` recheck rebuilds a
   corrupted venv rather than serving from a broken interpreter.
2. **Launch** (`launcher.rs` / `main.rs`): the supervisor spawns
   `trusty-embedderd-py --stdio`; the launcher ensures the venv (cheap
   torch-free `.ready` recheck on respawn) then **`exec`s** the venv's
   `python -m trusty_embed_sidecar --stdio`, inheriting stdio and env.
3. **Serve** (`python/trusty_embed_sidecar`): a newline-JSON-RPC-2.0 stdio
   server speaking the EXACT `trusty-embedderd` wire protocol — reader/worker
   split (multi-flight safe, MPS-serialized), id echoed verbatim, stdout =
   frames only / logs to stderr, empty batch → `[]`, EOF/SIGTERM → clean exit
   under a progress-aware shutdown watchdog.

Because the sidecar impersonates `trusty-embedderd` byte-for-byte, trusty-search
drives it through the unchanged `EmbedderSupervisor` / `StdioEmbedderClient` /
`LazyEmbedderHandle` — **zero** supervisor/stdio/protocol wire-code changes.

**Full architecture, device selection, idle-shutdown lifecycle, Linux-CUDA
build notes, and validated benchmark numbers:**
[`docs/trusty-search/research/python-mps-embedder-sidecar-2026-07-20.md`](../../docs/trusty-search/research/python-mps-embedder-sidecar-2026-07-20.md).
User-facing env reference: trusty-search `CLAUDE.md` → "Python/MPS sidecar
tuning".

### Environment

| Variable | Purpose |
|----------|---------|
| `TRUSTY_EMBEDDER=python` | Select this sidecar (opt-in). |
| `TRUSTY_DEVICE` | `cpu` \| `gpu` \| `auto` (default; MPS on Apple Silicon). |
| `TRUSTY_PY_EMBED_FP16=1` | fp16 mode (fp32 default; cosine still ≥0.9999). |
| `TRUSTY_PY_EMBED_BATCH_SIZE` | Python-side batch override (else forwarded `TRUSTY_EMBED_BATCH_SIZE`). |
| `TRUSTY_UV_BIN` | Explicit `uv` path (else PATH). |
| `TRUSTY_PY_BOOTSTRAP_TIMEOUT_SECS` | Per-step bootstrap timeout (default 600). |
| `TRUSTY_EMBEDDERD_PY_BIN` | Explicit launcher path (else sibling/PATH). |

## Tests

```bash
# Rust unit tests (no torch/venv):
cargo test -p trusty-embedderd-py

# Python protocol conformance (torch-free):
cd python && PYTHONPATH=. python -m pytest tests/

# Real-model correctness gate (needs torch + a built model):
cd python && TRUSTY_RUN_REAL_MODEL=1 PYTHONPATH=. python -m pytest tests/ -m real_model

# Full real-venv e2e through the real supervisor:
TRUSTY_RUN_PY_E2E=1 cargo test -p trusty-embedderd-py --test e2e -- --ignored
```
