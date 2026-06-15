# CI Quality Gate Plan for trusty-search — Issue #129

## Status: PLANNED (not yet implemented)

This document describes why a lightweight GitHub Actions CI gate for
trusty-search relevance (MRR@5 / Recall@10) is not yet wired up, what is
blocking it, and the concrete path forward.

---

## What the tests do today

`crates/trusty-search/tests/baseline_trusty_tools.rs` and
`tests/benchmark_synthetic.rs` (the `--include-ignored` regression tests)
currently:

1. Connect to a **live HTTP daemon** (`trusty-search start`) already running on
   the developer's machine.
2. Submit search queries against a fully-indexed corpus (trusty-tools, ~17k–21k
   chunks for `baseline`, or the 42-file synthetic corpus for `benchmark_synthetic`).
3. Measure Hit@1, Hit@5, MRR, and query latency against known-good thresholds.
4. Assert that relevant files appear in the top-k results.

None of this can run in a standard GitHub Actions runner as written, because:

- There is no daemon process in CI.
- The ONNX model download (~22 MB) requires the HuggingFace CDN and is
  rate-limited (issue #865 already added a model-cache step for unit tests, but
  embedding at reindex scale takes 2–10 minutes).
- Indexing 42 synthetic files with a real ONNX model takes 30–120 s on a
  standard runner (cold ONNX arena + model init + embedding).

---

## What's already in CI

The existing CI job (`ci.yml`) runs:

- `cargo test --workspace` — covers 1100+ unit and integration tests, **zero
  ONNX/daemon dependencies**.  All tests that require the model are marked
  `#[ignore]` (see `embedder_supervisor_e2e.rs`, `baseline_trusty_tools.rs`).
- BM25 correctness, query classifier, chunker, KG builder, HNSW round-trips, and
  schema migration tests all pass without a running daemon.

---

## Concrete blocking constraints for a CI regression gate

| Constraint | Notes |
|---|---|
| Daemon startup | `trusty-search start --foreground` requires a real binary, a writable data directory, and SIGTERM handling. Achievable on Actions but adds ~30 s overhead per run. |
| Model download | `AllMiniLML6V2Q` ONNX (~22 MB) must be present in the model cache. The existing test job pre-seeds it, but only into `~/.cache/fastembed`. The daemon path uses the same cache dir at runtime, so this is solvable. |
| Reindex time | 42 synthetic files × ~8 chunks/file ≈ 336 chunks. At 20 chunks/s on a 2-core runner, embedding takes ~17 s. Feasible but risky for flakiness. |
| Relevance thresholds | Current thresholds (Hit@1, Hit@5, MRR) were set against the developer's hardware. Cold-runner ONNX may produce slightly different embedding values (SIMD paths differ), which could shift ranks. A tolerance band needs to be defined. |
| BM25 circular bias | `baseline_trusty_tools.rs` runs against the trusty-tools corpus itself, which contains the query strings verbatim — inflating Hit@1/MRR artificially (issue #123). Only `benchmark_synthetic.rs` (non-circular corpus) is safe for a quality gate. |

---

## Recommended path: lightweight synthetic-corpus gate

The **synthetic corpus** (`tests/benchmark_corpus/synthetic/`, 42 `.rs` files)
is the right target for a CI gate because:

1. No circular BM25 bias — symbol names are unique to the corpus.
2. Small enough to reindex in <30 s with the model pre-cached.
3. Ground truth is checked in (`GROUND_TRUTH.json`) and version-controlled.

### Implementation plan (3 steps)

#### Step 1 — Port `benchmark_synthetic.rs` to an in-process harness

Currently the test connects to a live HTTP daemon.  Refactor it (or add a
parallel test) to use the library API directly:

```rust
// in-process harness (no daemon required)
let indexer = CodeIndexer::new("synthetic-gate", corpus_path);
let handle = Arc::new(IndexHandle::bare(...));
// run_reindex() from the library API
// then call indexer.search() directly
```

This eliminates the daemon startup requirement entirely. The ONNX model is still
needed, so the test must remain `#[ignore]` OR the CI job must opt into ONNX
via `--include-ignored`.

Tracking: requires `CodeIndexer::search` to be callable without an HTTP layer —
it already is; the gap is wiring `spawn_reindex` + polling stages from a test.
See `service/reindex/tests.rs` for the pattern (`reindex_walks_directory_and_emits_events`).

#### Step 2 — Add a `regression-gate` CI job

In `.github/workflows/`:

```yaml
regression-gate:
  name: Search quality gate (synthetic corpus)
  runs-on: ubuntu-latest
  needs: [fmt, clippy]
  env:
    SKIP_UI_BUILD: 1
    TRUSTY_EMBEDDER: in-process
    RUST_LOG: warn

  steps:
    - uses: actions/checkout@v4

    - uses: dtolnay/rust-toolchain@stable

    # Re-use the model cache from the test job.
    - name: Cache fastembed model
      uses: actions/cache@v4
      with:
        path: ~/.cache/fastembed
        key: fastembed-AllMiniLML6V2Q-${{ hashFiles('Cargo.lock') }}

    - name: Pre-seed ONNX model
      run: |
        cargo test -p trusty-search -- --include-ignored bm25_smoke 2>/dev/null || true
        # The above run triggers model download if cache miss.

    - name: Run synthetic-corpus quality gate
      run: |
        cargo test -p trusty-search --test benchmark_synthetic \
          -- --include-ignored --nocapture
      timeout-minutes: 10
```

**Threshold policy**: gate must fail if Hit@1 < 0.40 or MRR < 0.50 on the
synthetic corpus (current baseline: Hit@1 ~0.43, MRR ~0.57 per
`synthetic-corpus-baseline-2026-05-25.md`).  A 5-point regression triggers
CI failure.

#### Step 3 — Hardened assertions in the harness

Once the harness is in-process, add Rust `assert!` calls (not just `println!`)
so the job fails fast on a regression rather than printing a warning and exiting
0.  Example:

```rust
assert!(
    hit_at_1 >= 0.40,
    "Hit@1 regression: {hit_at_1:.2} < 0.40 threshold"
);
assert!(
    mrr >= 0.50,
    "MRR regression: {mrr:.2} < 0.50 threshold"
);
```

---

## Why this is not implemented yet

The primary blocker is Step 1 — porting `benchmark_synthetic.rs` to an
in-process harness. That port requires:

1. A library-level `run_reindex` entry point that works without an HTTP daemon
   (it exists in `service/reindex/tests.rs` as a test helper, but is not exposed
   via a clean public API).
2. Deciding whether to accept the ~30–60 s ONNX model-init time on every PR
   (risky for CI tail latency) or to fall back to `lexical_only=true` for a
   faster-but-weaker BM25-only gate.
3. Defining a stable set of thresholds that do not vary across runner hardware.

Until those decisions are made and the port is implemented, the regression test
suite remains a manual process run by maintainers before each release (per the
README in this directory) and tracked in the `#129` tracking issue.

---

## Related tickets

- **#129** — tracking issue for benchmark results across releases
- **#123** — BM25 circular bias (blocks using `baseline_trusty_tools.rs` in CI)
- **#865** — model cache for CI (already implemented for unit tests)
- **Q4** in `docs/trusty-search/spec/PRD.md` — open question tracking this gate

---

*Written: 2026-06-14. Update this document when the gate is implemented.*
