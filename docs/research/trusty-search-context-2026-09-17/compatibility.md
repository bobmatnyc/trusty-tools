# Search experiment compatibility

The context experiment preserves existing corpus and search contracts with the default settings. No full source reindex is required to load or search an existing corpus. Context gains arrive when a file is next indexed; an optional reindex applies them to unchanged files.

## Evidence

- `crates/trusty-search/src/core/experiment.rs:35`: omitted settings retain context budget 0 and subchunk window 100. The configuration is fixed once per process; changing settings requires a daemon restart, not an index rebuild.
- `crates/trusty-search/src/core/experiment.rs:109`: zero context is a strict no-op. Enrichment appends terms to the already-existing `RawChunk.virtual_terms`; it does not replace source, coordinates, relationships, or chunk IDs.
- `crates/trusty-search/src/core/chunker/types.rs:108`: `virtual_terms` already has `#[serde(default)]`. Neither this persisted type nor the corpus schema changed.
- `crates/trusty-search/src/core/migration/mod.rs:60` and `crates/trusty-search/src/core/corpus/tables.rs:118`: schema version remains 5 and KG graph format remains 1. No migration registration, version bump, invalidation fingerprint, or automatic reindex trigger was added.
- `crates/trusty-search/src/core/indexer/ingest/mod.rs:338`: the ordinary per-file ingestion path applies context only to supplied new/changed content. Unchanged corpus rows are not rewritten. Bulk ingestion uses the same enrichment helper.
- `crates/trusty-search/src/core/indexer/persist.rs:274`: existing warm restoration reads persisted rows and rebuilds the in-memory BM25 view; it loads/rebuilds KG from persisted data. This existing memory initialization is not a full source walk or re-chunking.
- `crates/trusty-search/src/core/indexer/persist_hnsw.rs:248`: old and enriched rows both use the same BM25 document composition already supported before the experiment.
- Existing SearchQuery, CodeChunk, HTTP search, and MCP response schemas are untouched. The cards endpoint belongs only to the custom example daemon; lean TOC and query/KG routing belong to the experiment harness.

## Added regression

`crates/trusty-search/src/core/indexer/tests/experiment_no_vector.rs:135`, `experiment_existing_corpus_accepts_incremental_context_without_reindex`, uses three fresh subprocesses to avoid changing global environment/configuration during parallel tests:

1. Create a persisted, unenriched corpus through the real ingestion path with budget 0.
2. Reopen it with budget 128. Assert identical serialized search results before ingestion. Add a context-enriched file and update it through `index_file`. Assert every untouched old row is byte-equivalent after JSON serialization and existing chunk IDs remain stable. Assert lexical and KG searches find both old source and a term present only in new chunk context.
3. Reopen the mixed corpus with budget 0. Assert all rows remain readable/searchable and the enriched terms persist.

No source files are written on disk. The test operates only on persisted corpus rows and text passed to `index_file`. Each phase uses a rejecting embedder and checks zero embedding calls and zero stored vectors. It also checks that the KG contains the old and new symbols.

The regression uses baseline configuration to produce the old representation; it does not run a separately compiled pre-change binary. Source inspection confirms that its persisted representation and schema are unchanged.

## Scope and rollout constraints

Keep the 100-line window as the compatible default and selected configuration. The alternative 64-line experimental window deliberately changes subchunk coordinates; its output should not be described as unchanged chunk boundaries.

Disabling enrichment prevents future additions but does not erase previously persisted terms. Mixed coverage is valid and can affect rankings as files gradually gain context. Reindexing is optional for uniform coverage, not a startup requirement.

Production query normalization, directed KG ranking, and an optional lean TOC response still require implementation and API compatibility validation. This branch preserves the experiment and does not silently enable those policies in production.

No production source changes were required by this compatibility review. Reused the existing corpus fixture, real `index_file`, redb persistence/restore, lexical/KG search, and rejecting embedder. Added: 131 lines / Removed: 0 / Net: +131 in the existing test file.

## Verification Results

Status: VERIFIED WORKING for persisted-corpus compatibility and incremental enrichment under the tested 100-line window.

All commands ran in `/Users/masa/trusty-search-experiment/worktree` with:

```sh
PATH=/Users/masa/.rustup/toolchains/1.94.1-aarch64-apple-darwin/bin:$PATH
CARGO_HOME=/Users/masa/trusty-search-experiment/cargo-home
CARGO_TARGET_DIR=/Users/masa/trusty-search-experiment/target
RUSTC=/Users/masa/.rustup/toolchains/1.94.1-aarch64-apple-darwin/bin/rustc
RUSTC_WRAPPER=''
SKIP_UI_BUILD=1
```

- `cargo check -p trusty-search --offline --locked -j6`: `check EXIT=0`.
- `cargo clippy -p trusty-search --all-targets --offline --locked -j6 -- -D warnings`: `clippy EXIT=0`.
- `cargo test -p trusty-search --no-fail-fast --offline --locked -j6 experiment_existing_corpus_accepts_incremental_context_without_reindex`: `test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 2208 filtered out; finished in 0.30s`.
- `cargo test -p trusty-search --no-fail-fast --offline --locked -j6`: `tests EXIT=0`; 36 target summaries sum to 2835 passed, 0 failed, 43 ignored. Main library raw result: `test result: ok. 2206 passed; 0 failed; 3 ignored; 0 measured; 0 filtered out; finished in 57.05s`. CLI raw result: `test result: ok. 480 passed; 0 failed; 3 ignored; 0 measured; 0 filtered out; finished in 6.21s`. All other integration targets also completed because `--no-fail-fast` was used.
- `cargo fmt --all --check`: `fmt EXIT=0`.
- `bash scripts/check_line_cap.sh crates/trusty-search/src/core/indexer/tests/experiment_no_vector.rs`: `EXIT=0`.
- `git diff --check`: exit 0. Literal newline escape bytes in source fixtures were checked directly and passed.

The 43 ignored cases already exist: optional 100k performance measurement, machine-specific migration fixture, installed-daemon integration checks, and hardware/model soak coverage. No test was ignored or narrowed to obtain a pass. Hardware embedding tests were not enabled, consistent with the experiment's no-embedding scope.

Logs: `/Users/masa/trusty-search-experiment/results/compatibility-{check,clippy,targeted,tests,fmt,line-cap}.log`.

Next: the parent packages this report with the research, commits/pushes the existing isolated branch, and records production completion requirements on the tracking ticket. No daemon or installed binary changed during this review.
