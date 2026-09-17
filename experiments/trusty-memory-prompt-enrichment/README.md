# Resident memory prompt enrichment experiment

Compare factual prompt context from BM25, graph lookup, and cached local embeddings.

## Run

From this directory, with the pinned dependencies installed in a dedicated environment:

```sh
python evaluate.py --helper /absolute/path/to/memory_prompt_probe \
  --model-dir /absolute/path/to/cached-MiniLM-snapshot --output results/run-01
```

The output directory must not exist. The runner verifies the frozen manifest before
loading source data. It writes tuning results and policy selection before parsing
`gold-heldout.json`. No model, tokenizer, or other resource downloads are permitted.
A local cl100k tokenizer cache is required. `offline_encoding.py` is copied unchanged
from the preceding deterministic memory experiment; it verifies pinned cache bytes
and has no network fallback.

## Install and verify

```sh
python -m pip install -r requirements.txt
cargo build -p trusty-memory --example memory_prompt_probe --offline --locked
MEMORY_PROMPT_HELPER=/absolute/path/to/memory_prompt_probe \
MEMORY_PROMPT_MODEL=/absolute/path/to/cached-MiniLM-snapshot python -m pytest -q
python -m mypy --strict records.py adapters.py projection.py retrieval.py scoring.py evaluate.py scaling.py packet_integrity.py
```

Dependency installation is a setup step outside measurement. Tests need the real
local Rust helper and fp32 ONNX model; they do not skip missing files. Model and
tokenizer hashes are pinned in `adapters.py`. Inference uses one CPU thread, maximum
256 model tokens, attention-mask mean pooling and L2 normalization. Prompt budgeting
independently uses cl100k_base.

## Measurement

The Rust JSONL process keeps each clock/scope projection resident. It calls actual
`BM25Index`, `KnowledgeGraph` APIs and the public prompt formatter. Native graph
construction uses `import_all`, then closes and reopens the graph before timing.
Hydration is checked against native functional-predicate semantics: stored active
row counts and hydrated edge counts can differ. Both counts are retained.

The bounded graph uses exact entity/alias seeds and an index from query tokens to
possible entity names. Ambiguous aliases supply no seed. Entity relationships consume
hops; literal assertions are properties of reached entities. Every inspected assertion
consumes the scan budget. Native traversal retains native edge-hop semantics and has
no examined-edge cap. Structural plumbing predicates are excluded only from the new
graph, as declared before evaluation.

All four main treatments rank identical contextual source text and pack complete
extractive claims. RRF has fixed k=60. The common standing prelude consumes the same
128/256/512-token budget and does not count toward task quality. The full-source
ablation removes ineligible fact spans before including a source body. The native
lexical graph control calls the actual newest-200 page API, then reproduces the
existing private hot-predicate/lexical-overlap selector with top-k=8.

Standing cache is a scope-adapted synthetic cache, preformatted per budget before
measurement. Its measured query performs an in-memory lookup. Native controls verify
complete rendered public-formatter bullets; main treatments verify complete source
claims. Their representations must not be described as identical.

Task macro F1 and coverage use positive task queries only. F1 is the harmonic mean
of accepted-emitted precision and required-group coverage. Alternate evidence within
a required group earns one coverage credit. Negative queries have no coverage/F1;
empty rate and unsupported task tokens are separate. Standing-only queries are
excluded from task aggregates. Standing metrics remain in each packet record.
Standing coverage uses the eligible standing identities listed in gold acceptable,
including standing-only queries whose required-task groups are empty. Its explicit
`standing_expected_facts` denominator excludes expired or out-of-scope assertions.
Overall task precision averages non-standing query precisions: an empty negative
packet has precision 1, while an empty positive packet has 0.

Five measured repetitions follow one warmup for heldout comparisons. Query order
rotates across treatments. Repetitions measure timing, not independent quality samples.
Native API time and IPC-inclusive packet latency remain distinct. Scaling uses a
fixed 512-edge substantive hub, a one-edge seed, and 100/1000/10000 unrelated edges.
It measures latency, not new relevance evidence. RSS is the experiment-process peak,
not per-treatment or total helper-process memory.

## Maintenance and limits

Source/vector maintenance publishes complete batches only after every encoding succeeds.
Mixed snapshots retain lexical access to sources whose vectors are absent. A stable
bounded backfill fills those missing vectors without rewriting sources.
Source/vector maintenance updates only changed sources in bounded event batches.
Deletion removes vectors and source records. No-op cycles retain vector objects and
factual timestamps. Incremental vectors are compared with a clean rebuild at absolute
tolerance 1e-6. Finite clock/scope BM25 and graph projections are reconstructed outside
query timing. This does not establish bounded total maintenance, incremental native
publication, production migration compatibility, or live latency.

Graph edges are curated fixture assertions. This measures their potential, not
automatic relation extraction quality. No answer model is used: quality measures
factual evidence available in the prompt, not generated answer accuracy. Protocol,
input hashes, packets, selected policies, model identity, and source hashes are saved.
The scorer independently reconstructs exact text from authoritative source facts and
the public formatter, rejects unregistered or replaced assertions, and recounts tokens.
`truncation_events` counts all corpus-encoding truncations, including clean-rebuild
validation and event updates; it is not a unique-source count. The supplied helper
is built with Cargo dev/debug settings; its timings are not release benchmarks.

## Reference and contributions

See the [frozen protocol](../../docs/research/trusty-memory-prompt-enrichment-2026-09-17/protocol.md)
and [interface](../../docs/research/trusty-memory-prompt-enrichment-2026-09-17/interface.md).
Change fixtures only before a new frozen run. Preserve superseded runs. This experiment
inherits the repository's MIT license. No production memory source, live palace,
daemon, or configuration is modified.
