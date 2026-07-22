# Changelog

All notable changes are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

---
## [0.1.1] — 2026-07-21

Patch release closing unpublished source drift under the already-published
0.1.0 (issue #3366 defect class): the epic #3524 slice 5 device-echo change
below landed on `main` after 0.1.0 was published to crates.io without a
version bump, so the live 0.1.0 tarball (this crate embeds its Python
payload via `include_dir!` at publish time) does not contain it. Published
now as the direct dependency of trusty-search 0.38.0's slice 6 default flip,
whose live `/health` provider readback relies on this field.

### Added

- **Response frames echo the actual resolved torch device** (epic #3524
  slice 5, issue #3493 P1) — `build_encoder` now attaches the resolved device
  (`"mps"` / `"cuda"` / `"cpu"`) to the returned encoder callable as a
  `.device` attribute, and `protocol.handle_frame` echoes it as an optional
  `result.device` field on every success frame when the encoder sets one.
  Lets `trusty-search`'s `/health` report the real device instead of a
  build-features prediction. Omitted entirely when the encoder has no
  `.device` attribute, so the wire shape is unchanged for any torch-free
  stub encoder (e.g. the protocol conformance tests).

## [0.1.0] — 2026-07-20

Initial release — opt-in Python/MPS embedding sidecar launcher for
trusty-search (epic #3524, slices 2-4). **DEFAULT-OFF**: only active when
`TRUSTY_EMBEDDER=python` selects it. Refs #3524, #3498, #3493.

### Fixed

- **HIGH: SIGTERM could hang the sidecar, needing SIGKILL.** The previous
  `SIGTERM` handler called `sys.stdin.close()` from inside the handler while
  the main thread was blocked in `sidecar.serve`'s `for line in stdin:` read —
  a reentrant call into the same (non-reentrant) `BufferedReader`, which raised
  a `RuntimeError` *inside the handler* that was swallowed by a bare
  `except Exception`, leaving the read (and process) hung forever. The handler
  now raises a dedicated `_ShutdownRequested` exception instead of touching
  stdin; per PEP 475 this propagates cleanly out of the interrupted blocking
  read, and `serve()`'s existing `finally` (drain the queue, join the worker)
  still runs during the unwind. Added a real-signal regression test
  (`test_signal_shutdown.py`) that spawns the sidecar as a subprocess with
  stdin held open and asserts a real `SIGTERM` produces a prompt, clean exit —
  the prior conformance suite only covered EOF via an in-memory `StringIO`,
  never a real signal.

### Added

- **Progress-aware shutdown watchdog.** `sidecar.py`'s shutdown path no longer
  bounds the worker-join with a flat timeout (which would force-exit a
  legitimately slow-but-healthy drain — e.g. a reindex burst queued right at
  SIGTERM — dropping in-flight, not hung, replies). The worker now marks a
  monotonic `_ProgressTracker` timestamp after each completed item;
  `_join_with_progress_watchdog` polls in short (1s) increments and only
  force-exits (`os._exit(1)`) once NO item has completed for 20s — a genuine
  wedge (e.g. a hung MPS `encode()` mid-batch), not merely a long drain. New
  tests (`test_shutdown_watchdog.py`) cover both: a slow-but-progressing
  worker drains fully without force-exit, and a truly wedged worker
  force-exits within the no-progress window.
- **`TOKENIZERS_PARALLELISM=false` by default.** Set (via `setdefault`, so an
  operator override is never clobbered) before `sentence_transformers`/
  `transformers` are ever imported, eliminating the extra
  `multiprocessing.resource_tracker` child HuggingFace tokenizers otherwise
  spawns and the associated "leaked semaphore" shutdown noise — a simpler
  process tree with one fewer orphan-prone child.
- **Two-tier `.ready` sentinel re-verification.** `.ready` is no longer
  trusted forever (a post-build corruption — a broken native `.so`, an ABI
  shift, a half-deleted directory — would otherwise route real traffic to a
  broken interpreter), but the recheck depth now depends on the caller:
  `ensure_venv()` — called by the `trusty-embedderd-py` launcher binary on
  EVERY respawn — uses the CHEAP, torch-free `verify_venv_alive` (interpreter
  liveness + an installed-package marker-file check, bounded to 5s, no
  `import sentence_transformers`) so a respawn never re-pays torch's import
  cost and undercuts the point of a longer idle-shutdown window.
  `ensure_venv_eager()` — called ONCE by trusty-search's daemon at `start` —
  uses the FULL `verify_full_import_smoke` (a real `import
  sentence_transformers`, bounded to 10s), since that cost is paid only once
  per daemon lifetime. A venv that fails its recheck is rebuilt instead of
  silently serving from a broken interpreter; if the rebuild itself fails, the
  error propagates so `commands/start/embedder.rs`'s existing
  fall-back-to-ort path fires.

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
