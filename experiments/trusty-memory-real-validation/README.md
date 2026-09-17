# Real memory validation

This experiment evaluates retained historical user prompts against a current memory
snapshot. It reuses frozen retrieval policies and the existing native BM25 helper.
Embeddings remain off. It cannot reconstruct historical source versions.

The exporter uses `ReadOnlyRedb` and one read transaction. Drawer chunks preserve
exact UTF-8 byte spans, capped at 80 tokens. Graph assertions retain native predicates
and their own record provenance. A graph assertion is not a drawer excerpt. Opaque
drawer subjects and unsupported native predicates may prevent frozen selectors from
using otherwise relevant text; report that applicability limitation.

All corpus, prompt, judgment and result files belong in a private directory outside
Git worktrees. Outputs are exclusively created with mode 0600. Source and tests contain
only generic code and toy data. Never commit content-bearing outputs.

Build the separate exporter with the existing target cache:

```sh
CARGO_TARGET_DIR=/Users/masa/trusty-search-experiment/target cargo build -p trusty-memory --example memory_snapshot_export --locked
```

Its required arguments are `--database`, `--private-output`, and `--max-rows`.
The database must already exist. Any undecodable row or exceeded bound fails extraction.
The four historical drawer layouts retain explicit `decode_version`, `absent_fields`,
and aggregate version counts. Decoding requires an exact byte round-trip; missing
historical fields remain distinguishable from recorded null fields.

Run `real_evaluate.py prepare --snapshot SNAPSHOT --prompts SAMPLE --sample-sha256 HASH
--scope PALACE --private-output NEW_DIRECTORY`. Preparation loads the frozen 32-query
sample and writes `prepared.json` without creating retrieval indexes or ranking.
Independent judges use its source bodies and evidence metadata to label supporting
drawer byte spans. The sample hash is the complete file SHA256, not an array hash.

Run `real_evaluate.py evaluate --prepared PREPARED --prepared-sha256 HASH --gold GOLD
--gold-sha256 HASH --helper HELPER --private-output NEW_DIRECTORY` after gold freezes.
Gold contains a `judgments` array with `query_id`, `status`, `required`, `supporting_spans`,
`acceptable`, `forbidden`, and `corroboration`. Each span has `source_id`, `start_byte`,
and `end_byte`. Status is positive, negative, unavailable or ambiguous. Corroboration
pairs are graph evidence ID followed by drawer evidence ID. Every accepted graph
assertion requires corroboration. Rationale text may be retained privately.

Every positive needs nonempty `required` groups of alternative evidence IDs. Each
group expresses one mandatory requirement; each member must independently satisfy
that requirement. A required quote spanning several chunks needs one mandatory group
per needed chunk. Relevant drawer IDs also need exact overlapping supporting spans.
`acceptable` IDs are optional support and do not replace required groups. Every positive
contributes to group coverage and full-query completion, including empty packets.

The three arms are raw BM25, frozen old combined, and frozen structured plan. They
use the same adapted corpus and packer at 128 and 256 tokens, but different candidate
acquisition; differences do not isolate causal graph value. Report note recall,
complete supporting-span coverage, union supporting-byte coverage, drawer relevant-chunk
fraction, corroborated KG precision, applicability,
tokens and latency. Unavailable and ambiguous labels do not count as negatives.
Unresolved graph records are counted separately and excluded from judged precision.
The inherited packet validator checks source spans, rendered text and budgets outside
the timed interval.
P95 uses nearest-rank selection over three timings after one warmup per case.

```sh
/Users/masa/trusty-search-experiment/venv/bin/python -m pytest -q experiments/trusty-memory-real-validation/test_real_validation.py
```

From this experiment directory, strict type checking uses the inherited module paths:

```sh
MYPYPATH=../trusty-memory-query-plan:../trusty-memory-relevance:../trusty-memory-prompt-enrichment /Users/masa/trusty-search-experiment/venv/bin/python -m mypy --strict real_adapter.py real_evaluate.py
```

Design and interpretation: [research](../../docs/research/trusty-memory-real-validation-2026-09-17/).
