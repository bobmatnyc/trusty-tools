# Verification and reproducibility

Final experiment gates passed: nine Python tests, strict mypy over two production modules, five Rust exporter tests, exporter build, rustfmt, Rust line-cap and test-pointer checks. The independent code review is APPROVE. Security found no CRITICAL/HIGH issues and one MEDIUM temporary-file condition, mitigated for the actual run. An optional Clippy dependency check was canceled deliberately to prioritize the changed exporter tests; it is not a passing gate.

## Repairs before measurement

The first review identified mixed drawer/uncorroborated-KG denominators, missing required-group completion and missing union byte coverage. All were corrected before ranking. Subsequent review caught an empty-drawer failure and permissive triple decoding; regression tests failed before each repair and passed afterward. The exporter now accepts only the four canonical drawer layouts and requires exact encoding round trips for drawer and triple values. Active/history malformed and trailing graph bytes fail without an output artifact. These changes repair extraction and measurement, not the frozen retrieval policies.

The first real export stopped on a known older drawer layout. No partial output or temporary database remained. Compatibility decoding then exported the complete corpus. After the strict triple-decoder repair, a fresh read-only export produced identical drawer and graph contents. The later prepared corpus had identical evidence metadata, source text, IDs, query list and eligibility counts. Only the snapshot clock/provenance changed. Labels were frozen against the final strict corpus before ranking.

The parent independently reconstructed eligible drawers/triples from exported raw timestamps and checked exact content, UTF-8 offsets, nonoverlap, coverage of nonwhitespace source bytes, metadata and row counts. The independent judge wrote supporting spans before seeing rankings; the parent reviewed every quoted passage and rationale. A separate result audit recomputed coverage, completion, byte unions, note recall, timing and temporal strata without importing the scorer. It found zero discrepancies across 192 cases.

## Actual read-only and privacy boundary

Both exports reported direct read-only database access (`recovered_copy=false`). The exporter opens through ReadOnlyRedb and uses one read transaction; it does not construct the live writable store, application state or a model. Fixture tests verify unchanged database bytes and refusal to create a missing database. Live file hashes are not used to infer no mutation while other writers may run.

The experiment used a mode-0700 private root outside Git, mode-0600 content-bearing files, explicit private TMPDIR for exporter/helper, and an explicit existing tokenizer-cache directory. The offline loader checks the pinned tokenizer hash and has no download fallback. The first prepare attempt with only private TMPDIR failed because that changed the default cache path; setting the existing cache path resolved it without network access. No database or retrieval-index residue remained after the run. Five tool-hook files were retained inside the private root, not deleted as if owned by the evaluator.

Synthetic security checks blocked Python network access, model constructors and private-input reads, and limited test subprocesses to the existing helper. Native-helper no-model/no-network behavior was source-reviewed; Python guards do not sandbox native children. No dependency audit or production daemon verification is claimed. All raw prompts, note text, values, judgments and per-query output remain private; only aggregates and hashes are committed.

## Reproduction

Use the existing experiment virtual environment and the unchanged memory_prompt_probe helper. The private input paths and hashes are recorded locally in frozen-inputs.json; exported content is intentionally not committed. The [source hashes](evidence/source-hashes.json), [run provenance](evidence/provenance.json), [input audit](evidence/input-audit.json), and [independent results](evidence/results-independent.json) identify the exact measured artifacts. The helper SHA256 remains `6e30d87ddd012025db474cfa67d6607bfebd1584368f9076ff0812fee204af25`, identical to the prior experiment.

The generic runner supports `prepare` and `evaluate`; see its [README](../../../experiments/trusty-memory-real-validation/README.md) for the private input schema and test commands. Supply explicit existing database/private-output paths to the exporter; set private TMPDIR and the pinned local TIKTOKEN_CACHE_DIR before launching either process. Freeze the sample and independent gold hashes before calling evaluate. Never replace this sample or tune the frozen policy after reading its outcomes.

All 145 prior experiment artifact hashes were checked unchanged. No production API/schema, live index, daemon, installation, embedding or model configuration was changed. No merge or production completion is claimed.
