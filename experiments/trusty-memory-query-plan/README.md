# Deterministic memory query plans

Three fixed treatments compare the previous combined policy, bundled defect repairs, and structured relation paths. Embeddings remain off. The [interface](../../docs/research/trusty-memory-query-plan-2026-09-17/interface.md) freezes vocabulary, grammar, limits, and interpretation.

Reuse the previous experiment's pinned requirements and existing `memory_prompt_probe` helper. From this directory:

```sh
python -m pytest -q test_query_plan.py
MYPYPATH=../trusty-memory-relevance:../trusty-memory-prompt-enrichment python -m mypy --strict plan_bridge.py plan_contracts.py plan_index.py plan_policy.py plan_execute.py plan_experiment.py plan_evaluate.py
python plan_evaluate.py --helper /absolute/path/to/memory_prompt_probe --output results/run-01
```

The evaluator requires `manifest.sha256` before any ranking. Entries use repository-relative paths, except the selected helper binary uses its absolute path. Freeze all new and reused production Python modules, source/query/gold JSON, research/interface/protocol documents, helper Rust source, and helper binary. Existing output directories are rejected.

The repaired and structured selectors share one BM25 plus plan-directed candidate pool. The repaired selector flattens relation requests and does not use structured support annotations. The control-to-repair contrast bundles candidate and selector changes. Independent gold determines recall; parser statuses only explain outcomes. The existing packer can discard support at a token boundary, so packet losses are reported separately.

This Python experiment does not modify production APIs, source clocks, persisted schemas, or installed daemons. Prohibition facts use curated `release_prohibition` assertions; no automatic prose polarity extraction is claimed.
