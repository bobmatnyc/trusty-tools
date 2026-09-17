# Deterministic memory experiment

Compare BM25 repair, structural context, chunking, temporal ranking, and scoped KG traversal using synthetic memories.

```sh
python3 -m venv .venv
.venv/bin/python -m pip install -r requirements.txt
# One-time setup downloads the public tokenizer table, not a model.
.venv/bin/python -c "import tiktoken; tiktoken.get_encoding('cl100k_base')"
.venv/bin/python -m pytest -q test_*.py
SKIP_UI_BUILD=1 cargo build --locked --manifest-path ../../Cargo.toml -p trusty-memory --example memory_deterministic_eval
.venv/bin/python evaluate.py --binary ../../target/debug/examples/memory_deterministic_eval --output scratch/new-run
```

Run from this directory with the repository Rust toolchain. If `CARGO_TARGET_DIR` is set, point `--binary` at that directory's example instead. Choose a fresh output directory; existing runs are never overwritten. The runner verifies the frozen input manifest before invoking the engine.

The engine takes explicit JSON source/index state and uses the repository's BM25 implementation. It discovers no daemon, opens no live memory database, and initializes no embedder or inference client. The normal dream cycle is not invoked: the example models a deterministic maintenance pass suitable for later integration.

See [results and limits](../../docs/research/trusty-memory-deterministic-2026-09-17/report.md), [research](../../docs/research/trusty-memory-deterministic-2026-09-17/README.md), [interface](../../docs/research/trusty-memory-deterministic-2026-09-17/interface.md), and [frozen evaluation protocol](../../docs/research/trusty-memory-deterministic-2026-09-17/evaluation.md). Run-01 is preserved but superseded for chunk-policy selection; [amendment-01](../../docs/research/trusty-memory-deterministic-2026-09-17/amendment-01.md) explains the v2 correction and prior holdout exposure.

## Evaluation

Seven predeclared KG configurations tune context budgets, chunk bounds, freshness weight, and KG weight. The selected configuration is written before held-out evaluation, then six cumulative ablations run on the held-out families. Source labels and validity errors are separate; empty-label questions do not receive artificial perfect recall. Held-out families share templates with tuning families, so this tests mechanisms, not general recall quality.

Each run preserves compressed responses/final state, publication work counts, repeatability hashes, source/binary/input hashes, selected policy, and summary metrics. Read an artifact with Python's `gzip.open` and `json.load`. Fixed-clock responses must be identical across fresh processes. Timed requests run three times per question after two batch passes; latency includes process launch, JSON decoding, validation, BM25 reconstruction, query, and response serialization. These measurements are not production daemon query latency. Child CPU is measured across the variant. Peak child RSS is a process-wide maximum over the full experiment, in native platform units; it is not per-treatment incremental memory.

Publication budgets bound selected maintenance work. This portable harness reads/validates the entire supplied state and reconstructs BM25 on each request; it does not prove production-wide CPU/memory bounds. Navigation cards preserve exact returned excerpts, source locators, and dates; token counts do not prove equivalent answer quality or fewer follow-up reads.

No private memories are part of the fixture. Production adoption still needs a daemon-side adapter, durable revision tracking, old-release/old-client tests, and broader real-query evaluation. Existing source stores and production behavior are unchanged by the example.

## Contributing and license

Follow the repository's contribution and review rules. Keep source labels frozen during tuning; changes require a new dataset version and a full rerun. Code uses the repository license.

Evaluation itself reads only the pinned local tokenizer table and fails closed if it is missing or corrupt. It has no download fallback.
