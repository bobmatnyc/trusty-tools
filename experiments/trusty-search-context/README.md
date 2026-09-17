# Trusty Search context experiment

Research, prototype, and reproducible evidence for chunking, BM25 context, query routing, graph traversal, and source navigation without embeddings. See the [research report](../../docs/research/trusty-search-context-2026-09-17/report.md) and [completion requirements](../../docs/research/trusty-search-context-2026-09-17/completion.md).

The production defaults remain unchanged. This directory contains experiment clients, not an installed replacement search API. The custom daemon is `crates/trusty-search/examples/context_experiment_daemon.rs`.

## Preserved work

All experiment Python programs and tests are included, along with the original frozen 48-query dataset. `evidence/` preserves 104 result, manifest, review, and log files, including all 23 accepted zero-vector result artifacts and rejected-run evidence. `evidence-manifest.json` records the original archive checksum, each compressed file checksum, and every uncompressed checksum. Original timestamps and host paths are retained as provenance; they are not portable runtime configuration.

Build caches, virtual environments, copied repositories, daemon state, and generated indexes are reproducible local outputs and are intentionally not committed. The original local experiment remains available as an additional backup.

## Offline evidence and tests

From this directory:

```sh
python3 -m venv .venv
.venv/bin/python -m pip install -r requirements.txt
.venv/bin/python unpack_evidence.py
.venv/bin/python -m pytest -q test_*.py
.venv/bin/python make_report.py
```

The reconstructed report is `results/report.md`; the project research copy is linked above. Archived runs support offline analysis only. They do not contain a writable corpus or live daemon and must not be used as replay fixtures.

## Fresh isolated experiment

From this directory, with Rust 1.94.1 available:

```sh
git archive 90b6aeb944e1d010f3690281ec24efe7b89435c3 -o source.tar
CARGO_TARGET_DIR="$PWD/target" SKIP_UI_BUILD=1 cargo build --locked --manifest-path ../../Cargo.toml -p trusty-search --example context_experiment_daemon
.venv/bin/python run_treatment.py --words 128 --window 100 --kg-cap 0 --name new-context-run
.venv/bin/python replay.py --run new-context-run --name new-replay --split tuning --variants original,normalized,directed_name
```

Choose fresh names and an unused explicit loopback port if the defaults are occupied. Each run creates marked disposable corpus/data directories, rejects embedding calls, verifies stage completeness, records evidence, and stops its own daemon. Replay requires a successful stopped source run and an exclusive corpus lock. Nothing discovers or changes the production daemon.

The dataset and selection scripts intentionally target the frozen revision above. Testing another repository revision requires new source labels and a fresh protocol; do not call reused held-out questions unseen data. The one-off orchestration scripts preserve the original experiment's fixed run names; use the individual runners for new runs.

## Compatibility boundary

Existing indexes remain readable/searchable without rebuilding. Source context reuses the existing optional `virtual_terms` field; missing terms are valid. With enrichment enabled, newly indexed or changed files can receive it incrementally, and old rows may remain unenriched. A full reindex is optional and gives full context coverage. Query-time routing and TOC presentation can operate on existing persisted chunks. Production integration, including safe fallback and API/version compatibility, is tracked separately in the completion requirements.
