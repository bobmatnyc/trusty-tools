# Lexical fallback and coherent passage experiment

This offline experiment compares three arms using the same exact prompt and native
top-20 mixed fixed-chunk candidates: unchanged raw BM25, lexical drawer selection
with fixed chunks, and the same selected seeds with coherent source passages.
The selector filters graph rows and applies frozen document-frequency/overlap
thresholds without typed parsing. Packing uses whole registered UTF-8 spans under
128/256-token budgets. Sentence and clause splitting applies only to oversized
prose/list units; fenced commands remain atomic.

The [protocol](../../docs/research/trusty-memory-passages-2026-09-17/protocol.md)
and approved interface define policy `memory-passages-v1`. Source-body digests
remain distinct from derived Source fingerprints. Independent span judgments use
the original source bytes. Every `supporting_spans` entry is mandatory; legacy
chunk IDs and `optional_spans` do not determine the primary outcomes.

`passage_policy.py` owns lexical selection, units, registered windows and packet
validation. `passage_evaluate.py` owns strict gold conversion, interval metrics,
timing and private output. Existing adapters, native index, canonical renderer,
packet validator and offline tokenizer are reused unchanged.

Use the existing virtual environment and frozen native helper. No Rust build,
network connection, embedding initialization, live memory export or live write
is part of this experiment. All inputs and content-bearing output stay private
outside Git. Set `TMPDIR` to an existing mode-0700 private directory. Pass the
pinned tokenizer cache explicitly; an absent or invalid cache fails without a
download. Output uses a new mode-0700 directory and exclusive mode-0600 files.

```sh
/Users/masa/trusty-search-experiment/venv/bin/python experiments/trusty-memory-passages/passage_evaluate.py \
  --prepared PRIVATE_PREPARED --prepared-sha256 PREPARED_SHA256 \
  --gold PRIVATE_GOLD --gold-sha256 GOLD_SHA256 \
  --helper /Users/masa/trusty-search-experiment/target/debug/examples/memory_prompt_probe \
  --cache PINNED_CACHE_DIRECTORY --output NEW_PRIVATE_DIRECTORY
```

The caller supplies approved paths and hashes after independent gold freezing
and code/security review. The engineer does not inspect held-out prompts or gold.
`cases.json` records native candidates, selected seeds, reachable expanded spans,
emitted registered facts, original body digests, reasons and three timing samples
after one warmup. Source/packet validation is outside those timing samples.
`summary.json` reports both budgets, positive support measures, negative-only
abstention, exclusions, timing percentiles and paired changes. Source creation
counts preserve before/equal, after and unknown strata; creation is not verification.
`provenance.json` records inputs/code/helper/encoding hashes, context, setup costs,
representable bytes/notes and oversized losses. Native candidate coverage never
credits adjacent context obtained during packing. Unannotated text remains
unresolved relevance; this is neither historical replay nor comprehensive precision.

From the worktree root, run the complete new experiment suite and reused adapter
regressions (synthetic inputs and the existing native helper only):

```sh
/Users/masa/trusty-search-experiment/venv/bin/python -m pytest -q \
  experiments/trusty-memory-passages/test_passage.py \
  experiments/trusty-memory-real-validation/test_real_validation.py
```

From this experiment directory:

```sh
MYPYPATH=../trusty-memory-real-validation:../trusty-memory-query-plan:../trusty-memory-relevance:../trusty-memory-prompt-enrichment \
  /Users/masa/trusty-search-experiment/venv/bin/python -m mypy --strict passage_policy.py passage_evaluate.py
```

The repository line-cap script covers Rust/Swift files. These Python modules use
the same 500-line cap with a direct nonblank/noncomment count. No prior experiment
code, fixtures, or results are modified.
