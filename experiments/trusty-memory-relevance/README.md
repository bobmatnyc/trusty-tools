# Deterministic memory relevance experiment

Separate relevance selection, relation lookup, claim indexing, and duplicate cleanup.

## Run

From this directory, using the previous experiment's installed environment/helper:

```sh
python evaluate.py --helper /absolute/path/to/memory_prompt_probe --output results/run-01
```

The output must be fresh. The parent freezes `query_policy.py` with the fixture,
interface, and protocol in `manifest.sha256` before any ranking. The runner writes
selector choice before parsing heldout gold. The four selector settings are reused
unchanged by the combined arm. No embeddings, model initialization, or network calls
are used; imports reuse the previous adapter module without constructing its encoder.

## Verification and setup

```sh
python -m pip install -r requirements.txt
MEMORY_PROMPT_HELPER=/absolute/path/to/memory_prompt_probe python -m pytest -q test_relevance.py
MYPYPATH=../trusty-memory-prompt-enrichment python -m mypy --strict \
  legacy.py contracts.py maintenance.py relevance_index.py query_policy.py \
  relation_support.py relevance.py input_data.py metrics.py evaluate.py
```

Dependencies are unchanged from the previous experiment. Dependency setup is outside
measurement. A valid local cl100k cache is required by the reused offline tokenizer.
The existing Rust helper is a debug build; timings do not establish release latency.

## Comparisons

The six arms are baseline, selector, relation_graph, claim_index, cleanup, and combined.
The baseline reuses source BM25 plus one-seed/one-hop generic graph. Each single arm
changes only its named stage; combined uses the fixed claim/relation policies and
selected selector, then cleanup. Source BM25 includes the original full source text;
claim BM25 repeats the source title with each claim. Their candidate caps are 20
sources versus 20 claims. Counts and explicit missing-derived fallback are retained.

Standing claims consume the same 128/256-token ceilings but remain outside task
quality. Every emitted claim retains its exact source span and revision. The reused
independent checker reconstructs exact text with the actual public formatter and
recounts tokens. Support-aware treatments remove detached endpoints when their full
support cannot fit. Baseline packet behavior is unchanged.

Duplicate identity is exact scope, subject, predicate, object, object entity, validity
bounds, source expiry and standing flag. Cleanup keeps the lowest evidence identity
at the earliest member rank, with complete member provenance. Distinct values/validity
are not merged. Precision credits unique acceptable semantic assertions against all
emitted task assertions; duplicate assertions still count in its denominator. Required
alternative groups count once. Candidate, postselection, and final coverage locate losses.
Duplicate token differences use actual same-formatter packets and can be negative if
freed space admits other content.

One warmup and three measured repetitions run for each query/budget/arm. Forward arm
order at 128 tokens reverses at 256 tokens. Timings exclude scoring, startup, finite
eligibility/index construction, and maintenance. Packets include stage counters,
rejections, complete support groups, source digests/spans, and provenance members.
Unique query counts, repeated query-budget cases, and timing samples are separate.
Requested demand metrics enumerate each intent–entity binding, including unresolved or
unsupported positive demands. Negated/hypothetical clauses are excluded. Full demand
completion requires all its bindings; retaining a selected support path is a separate
metric. These diagnostics use recorded support groups and therefore cannot establish
completion for baseline arms that do not annotate such groups. Gold all-required
coverage remains the definitive cross-arm task-success measure.

## Maintenance and limits

Per-source derived records contain claim documents, directed postings, aliases,
revision, digest and policy version. A batch stages all work before publication.
Tombstone revision watermarks prevent stale resurrection; replay is idempotent and
conflicting replay fails. Checkpoint roundtrip and clean-build equivalence are tested.
Missing, partial, or old-policy derived records retain native source-BM25 fallback.
No-op and backfill preserve source bytes and factual timestamps.

Source-count batch caps do not bound CPU/RAM: staging copies maps, validation scans
records, and finite scope/time native indexes are rebuilt outside query measurements.
The prototype does not establish production incremental publication or migration.
The frozen lexical rules intentionally miss unfamiliar wording and use coarse clause
binding. These misses and partial supported packets are results, not reasons to change
heldout labels or rules. Literal properties do not consume entity hops; examined facts
still consume graph budgets. Scope/time safety comes from prebuilt eligible projections.

## Reference, contributions, license

See the [protocol](../../docs/research/trusty-memory-relevance-2026-09-17/protocol.md)
and [interface](../../docs/research/trusty-memory-relevance-2026-09-17/interface.md).
Previous experiments remain immutable. Change policy/fixtures only for a new frozen
run and preserve superseded results. This experiment inherits the repository MIT
license. Production integration and installed verification remain tracked separately.
