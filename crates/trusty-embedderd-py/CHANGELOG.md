# Changelog

All notable changes are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

---
## [0.1.0] — 2026-07-20

Initial release — opt-in Python/MPS embedding sidecar launcher for
trusty-search (epic #3524, slices 2-4). **DEFAULT-OFF**: only active when
`TRUSTY_EMBEDDER=python` selects it. Refs #3524, #3498, #3493.

### Added

- **Python sidecar module `trusty_embed_sidecar`** (slice 2): a hardened,
  production stdio JSON-RPC 2.0 server speaking the EXACT
  `trusty-embedderd` wire protocol (`embed` method, newline-framed, id echoed
  verbatim, stdout = frames only / logs to stderr, empty-batch → `[]`,
  EOF/SIGTERM → clean exit). Multi-flight safe via a reader/worker split so a
  slow encode never blocks the stdin drain. Device auto-select
  (`TRUSTY_DEVICE=cpu|gpu|auto` → mps/cuda/cpu, MPS default on Apple Silicon),
  fp32 default with `TRUSTY_PY_EMBED_FP16=1` opt-in, forwarded
  `TRUSTY_EMBED_BATCH_SIZE` with an MPS unified-memory clamp, pinned model
  revision, model load + one MPS warmup BEFORE the first reply. 384-dim,
  unit-norm, all-zero-guarded. Ships a torch-free pytest protocol-conformance
  suite plus a `real_model`-gated ≥0.999 cosine correctness test.
- **Rust launcher crate `trusty-embedderd-py`** (slice 3): a signable Mach-O
  binary discovered by the sibling/PATH/`TRUSTY_EMBEDDERD_PY_BIN` logic. It
  ensures the venv (slice 4) then `exec`s
  `<venv>/bin/python -m trusty_embed_sidecar --stdio`, inheriting stdio and
  forwarding `TRUSTY_EMBED_BATCH_SIZE`. Reuses trusty-search's
  `LazyEmbedderHandle` / `EmbedderSupervisor` with ZERO changes to the
  supervisor/stdio/protocol wire code.
- **Robust uv/venv bootstrap** (slice 4): locates `uv`
  (`TRUSTY_UV_BIN` → PATH), `uv python install` (pinned CPython 3.11),
  `uv venv`, and `uv pip sync` against a hashed requirements file exported
  from a committed, hashed `uv.lock` (torch 2.9.1 + sentence-transformers
  5.6.0, resolved for BOTH macOS-arm64 and linux-x86_64). The venv lives at
  `resolve_data_dir("trusty-search")/py-embedder/<lockfile-hash>/venv` (never
  the repo; honours `TRUSTY_DATA_DIR_OVERRIDE`). Disk-space precheck (~3 GB),
  bounded timeout (`TRUSTY_PY_BOOTSTRAP_TIMEOUT_SECS`, default 600) + one
  retry on transient failure, `flock` against concurrent bootstraps, and a
  `.ready` sentinel (written only after `uv pip sync` + an import+embed smoke
  test; records the lockfile hash).

### Notes

- Building this crate does NOT require torch or a venv — that is all runtime.
- On ANY bootstrap failure trusty-search logs a loud warning and falls back to
  the Rust ort embedder, so search never hard-fails.
- Follow-ups (slices 5-7): vendored/download-with-SHA256 `uv` acquisition,
  doctor polish, packaging + default-on-Apple-Silicon, and the bench/#24 soak.
