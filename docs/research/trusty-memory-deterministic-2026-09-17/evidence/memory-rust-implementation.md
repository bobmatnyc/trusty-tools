# Deterministic memory Rust experiment

Implemented the isolated JSON/JSONL example in `crates/trusty-memory/examples/memory_deterministic_eval.rs`, with seven modules under `examples/support/memory_deterministic/` and a changelog fragment. Added: 1962 physical lines / Removed: 0 / Net: +1962, including tests. Every source module passes the project's 500-SLOC cap. The final v2 amendment and verification appear below.

The example reuses the real `trusty_common::bm25::BM25Index`, including insertion-capacity reporting and pre-truncation filtering. It accepts and returns explicit portable JSON state; it does not read/write live memory, instantiate a daemon/service container, use network access, initialize a model, or call embeddings/inference. There are no fabricated zero-call counters: that boundary is source-level evidence, not runtime instrumentation of production.

Maintenance preserves exact source bodies and revision provenance. It validates all inputs, orders revision/tombstone precedence, publishes each drawer's children together, persists a rolling cursor, detects metadata/body/dependency drift, and removes obsolete derived children. Missing derived metadata is valid legacy coverage. Unchanged maintenance does not rewrite documents or advance generation. The raw ablation intentionally retains same-ID stale indexed text to model existing ID-only backfill; it still filters deleted sources and marks stale hydration explicitly.

Retrieval compares raw/repaired/context/chunk/temporal/KG treatments. Scope, expiry and explicit recorded-time cutoffs precede ranking. Temporal intervals and declared single-valued fact slots govern current/as-of eligibility. Exact aliases remain independent KG candidates when the context cap excludes their terms. Ambiguous aliases remain separate, scoped candidates. Alias and one-hop neighbor additions share a 16-extra-candidate limit; at most 32 valid edges are selected. Original drawer IDs are grouped before final truncation, with stable ties.

State validation rejects current generations with missing, extra, or mismatched child rows/text. Source ranges and digests remain bound to their revision. When old indexed text has no available original revision, hydration returns the current complete source with `index_fresh:false`; it does not invent an old locator. A legacy snapshot array remains loadable by `PalaceBm25Index` without a schema conversion.

## Verification Results

All Cargo commands ran in `/Users/masa/trusty-search-experiment/worktree`, with:

```sh
PATH=/Users/masa/.rustup/toolchains/1.94.1-aarch64-apple-darwin/bin:$PATH
CARGO_HOME=/Users/masa/trusty-search-experiment/cargo-home
CARGO_TARGET_DIR=/Users/masa/trusty-search-experiment/target
RUSTC=/Users/masa/.rustup/toolchains/1.94.1-aarch64-apple-darwin/bin/rustc
RUSTC_WRAPPER=''
SKIP_UI_BUILD=1
```

- `cargo check -p trusty-memory --offline --locked -j6`: `EXIT=0`.
- `cargo test -p trusty-memory --example memory_deterministic_eval --no-fail-fast --offline --locked -j6`: `test result: ok. 13 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s`.
- `cargo clippy -p trusty-memory --example memory_deterministic_eval --offline --locked -j6 -- -D warnings`: `example-clippy EXIT=0`.
- `cargo build -p trusty-memory --example memory_deterministic_eval --offline --locked -j6`: `example-build EXIT=0`.
- `cargo test -p trusty-memory --no-fail-fast --offline --locked -j6`: `crate-tests EXIT=0`. The 35 target summaries sum to 1071 passed, 0 failed, 16 existing ignored. Main library raw result: `test result: ok. 936 passed; 0 failed; 4 ignored; 0 measured; 0 filtered out; finished in 55.30s`. CLI raw result: `test result: ok. 14 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.39s`.
- `cargo clippy -p trusty-memory --all-targets --offline --locked -j6 -- -D warnings`: `crate-clippy EXIT=0`.
- `cargo fmt --all --check`: `fmt EXIT=0`.
- Named-file `scripts/check_line_cap.sh` and `git diff --check`: exit 0.

The full crate suite and all-target clippy passed before the final isolated example review fixes. The final example tests/clippy/build/fmt ran again after those fixes. No production/core file changed. Existing ignored tests require default ONNX initialization or optional load/integration environments; no test was newly ignored.

The example tests cover same-ID body/metadata repair, unchanged no-ops, source preservation, bounded resume, serialization restart, legacy snapshot loading, tombstone revision ordering, scope filtering, temporal boundaries/history, deterministic ties, UTF-8 chunks, alias ambiguity, stale locators, capacity refusal, corrupt child ranges/sets, optional defaults, policy boundaries, and validation errors.

`production_backfill_skips_changed_text_when_id_is_present` invokes public `backfill_palace` and a temporary `Bm25Lane`: after indexing `oldterm` under one ID, a non-forced backfill of `newterm` under that same ID returns `AlreadyIndexed`; `newterm` is absent and `oldterm` remains searchable. This reproduces the maintenance gap in the checked-out implementation, not an observation of the installed daemon.

Final v2 binary: `/Users/masa/trusty-search-experiment/target/debug/examples/memory_deterministic_eval`.
SHA-256: `679b2842c2891afa9707a58fb68f4fd01ec7f9bdf91c267b68d4f42f09a0cd19`.
Logs: `/Users/masa/trusty-search-experiment/results/memory-{preflight-check,example-tests,example-clippy,example-build,crate-tests,crate-clippy,fmt,line-cap}.log`.

Status: VERIFIED WORKING for the isolated engine and tests. The parent owns benchmark measurements and final reviewer probes.

## Limits and remaining work

Publication budgets bound selected document, output-byte, and dependency-edge work. Stateless JSON validation, canonical fingerprint coverage calculation, full BM25 reconstruction, and state serialization still scale with the whole input corpus. This experiment does not demonstrate a bounded total-runtime production dream cycle.

No production adapter, authoritative memory migration, model-backed dream replacement, release, or deployment was made. Promoting this behavior requires a daemon-side incremental adapter, production source-change notifications, old-release/client compatibility fixtures, and workload evaluation beyond authored synthetic data. The existing reusable components are BM25Index, PalaceBm25Index, Bm25Lane, and public backfill; preserve those boundaries when integrating instead of copying scoring or persistence implementations.

Next: parent completes the independent final Rust review and frozen benchmark, then packages the research/evidence and records the production follow-up scope.

## v2 chunk-length correctness amendment

The first grid exposed a defect: `BM25Index` tokenization returns a deduplicated vocabulary. Using its whole-document count as chunk length left a 669-word repeated-text document unsplit. The v1 run is superseded; it is not valid evidence of chunk-size effects. The parent retains that run and discloses prior holdout exposure before rerunning the unchanged fixture, queries, and grid.

The confirmed replacement contract counts occurrences by summing `tokenize(run).len()` for every whitespace-delimited run, so repeated runs count repeatedly. Compound-identifier expansions count within each run. BM25 indexing/scoring retains its existing tokenizer. Each source-body child also has a fixed maximum of 4096 UTF-8 bytes, with scalar-boundary fallback for oversized individual runs. Added context has its separate budget. Raw and unsplit-context controls remain unchanged.

`policy_version` is now `memory-deterministic-v2`; the interface document records the changed boundary definition. This prototype does not silently reuse v1 derived metadata. Legacy production BM25 snapshot arrays remain the same portable representation.

Final v2 commands:

- `cargo check -p trusty-memory --example memory_deterministic_eval --offline --locked -j6`: `v2-example-check EXIT=0`.
- `cargo test -p trusty-memory --example memory_deterministic_eval --no-fail-fast --offline --locked -j6`: `test result: ok. 14 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.45s`.
- `cargo clippy -p trusty-memory --example memory_deterministic_eval --offline --locked -j6 -- -D warnings`: `v2-example-clippy EXIT=0`.
- `cargo build -p trusty-memory --example memory_deterministic_eval --offline --locked -j6`: `v2-example-build EXIT=0`.
- `cargo fmt --all --check`: `v2-fmt EXIT=0`.
- Named-file line cap and `git diff --check`: exit 0.

The new regression checks repeated words, compound identifiers, oversized multibyte single runs, whitespace-only input, exact contiguous UTF-8 coverage, occurrence/byte bounds, and unchanged unsplit controls. Real CLI smoke output:

```json
{"tokens":128,"children":6,"policy_version":"memory-deterministic-v2","max_body_words":128,"source_preserved":true}
{"tokens":256,"children":3,"policy_version":"memory-deterministic-v2","max_body_words":256,"source_preserved":true}
```

The earlier full crate gate remains applicable to untouched production code. Only example-scoped checks/tests/clippy/build/fmt ran after this example-only fix. Logs use `results/memory-v2-{example-check,example-tests,example-clippy,example-build,fmt,line-cap}.log`. No fixture, query, grid, production module, or installed binary changed.
