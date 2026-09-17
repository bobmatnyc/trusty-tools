## Verdict: BLOCK

## Findings

| Severity | File | Line | Issue | Fix | Disposition |
|----------|------|------|-------|-----|-------------|
| CRITICAL | experiments/trusty-memory-passages/passage_policy.py | 350–355 | `relevant = tuple(s for s in originals if s.source_id in wanted)` removes competing sources before `derive_passages(relevant, ...)` evaluates supersession. A forged view containing a superseded source passes validation against the complete original corpus. This breaks the promised independent source-eligibility validation. | Compute eligible evidence from the complete `originals` and task before narrowing reconstruction. Reject any selected seed absent from that eligible authority, including sources with partially ineligible facts. Add a negative test with an older selected source and a newer unselected source sharing `single_value_slot`. | Fix here |
| MEDIUM | experiments/trusty-memory-passages/passage_policy.py | 109–112 | `kinds = {metadata[i]['kind'] for i in ids}` is empty for a valid tombstone with `body=''`, `facts=()`, `deleted=True`; the function raises `invalid source kind`. The deletion contract requires exclusion, and the existing deletion test retains body/facts, which is not a valid parsed tombstone. | Exclude validated deleted/empty sources before checking fact metadata kinds. Test an actual tombstone accepted by `parse_source` through both lexicon and passage derivation. | Fix here |

## Required Changes

1. Preserve complete-corpus eligibility when validating selected passage seeds. Re-run the supersession probe and existing synthetic suite after repair.
2. Cover valid empty tombstones and exclude them consistently.

## Verification Results

Reviewed the Stage 1 research, Stage 2 interface and approved amendment, protocol, both implementation modules, README and synthetic tests. Current reused source code was inspected for eligibility, packet rendering/validation, indexing and private writers. No private sample, corpus or gold was read. No official ranking, Rust build, Git mutation or production change was performed.

Commands run from `/Users/masa/trusty-search-experiment/worktree`:

- `/Users/masa/trusty-search-experiment/venv/bin/python -m pytest -q experiments/trusty-memory-passages/test_passage.py experiments/trusty-memory-real-validation/test_real_validation.py > /tmp/passage-critic-tests.txt 2>&1; echo EXIT=$?` → `EXIT=0`.
- From `experiments/trusty-memory-passages`, `MYPYPATH=../trusty-memory-real-validation:../trusty-memory-query-plan:../trusty-memory-relevance:../trusty-memory-prompt-enrichment /Users/masa/trusty-search-experiment/venv/bin/python -m mypy --strict passage_policy.py passage_evaluate.py > /tmp/passage-critic-mypy.txt 2>&1; echo EXIT=$?` → `EXIT=0`.

Exact reproduction script: `/tmp/passage-critic-probe.py`. Run `/Users/masa/trusty-search-experiment/venv/bin/python /tmp/passage-critic-probe.py` from the worktree root. It uses `test_passage.corpus`, the existing offline tokenizer and frozen native helper. It assigns two original drawer sources the same `single_value_slot='exclusive'`, makes the second source revision 2, constructs a view/selection from the first source alone, and calls `validate_passages` with both originals. Raw output (exit 0):

```text
superseded_seed_eligible= False
superseded_packet_validation=ACCEPTED; emitted= 1
tombstone_parse=ACCEPTED
valid_tombstone= IntegrityError invalid source kind
```

The tombstone probe replaces the first synthetic source with `deleted=True`, `body=''`, `facts=()`, validates its JSON serialization with `parse_source`, then calls `build_lexicon`. An initial probe passed raw `asdict` tuples into the JSON parser and failed with `FixtureError: expected array`; the corrected probe uses a JSON round-trip and produces the output above.

Affected contracts: `docs/research/trusty-memory-passages-2026-09-17/interface.md:146` requires independent reconstruction from originals and rejection of fabricated views; `interface.md:173` requires deleted/superseded and partly ineligible drawer coverage; `research.md:61` explicitly preserves `eligible_facts` deletion/supersession behavior.

## Notes

The normal `run_case` index path excludes the superseded seed. The CRITICAL finding concerns the explicitly required independent validator guarantee: a caller can provide a matching forged view and packet, and the validator accepts ineligible source material. This is a reproduced contract failure, not evidence that the frozen official corpus contains affected records or that any measured retrieval result is wrong.

The interval scorer correctly unions overlaps and preserves gaps in the inspected code. Existing synthetic tests and strict typing pass, but they do not cover the reproduced supersession or valid-tombstone cases.

Parent continues: handle the BLOCK verdict and authorize the next pipeline step. Source remains frozen; no implementation edits were made.
