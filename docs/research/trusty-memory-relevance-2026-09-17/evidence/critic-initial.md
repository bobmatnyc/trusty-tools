## Verdict: WARN

One HIGH and two MEDIUM findings. No CRITICAL finding. Scope: frozen relevance experiment sources, specified interface/protocol/research, reused packet validation and native helper. No official ranking, benchmark, or heldout-gold execution; no source or Git changes.

## Findings

All paths below are relative to `/Users/masa/trusty-search-experiment/worktree`.

| Severity | File | Line | Issue | Fix | Disposition |
|----------|------|------|-------|-----|-------------|
| HIGH | experiments/trusty-memory-relevance/relevance.py | 75 | Unknown-intent fallback emits an alias-resolved endpoint without its alias assertion, even when that assertion was absent from selector candidates. The packet cannot establish the connection to the entity named in the query. | Build unknown-intent support groups with the alias assertions used for resolution; require those assertions in the candidate set, and apply the existing whole-group fact/token constraints. Add absent-alias and insufficient-budget regression cases. | Fix here |
| MEDIUM | experiments/trusty-memory-relevance/relation_support.py | 66 | New seeds are inserted into `visited` without its 32-node limit. A first seed can fill the visited set, after which two additional seeds raise the reported count to 34. | Enforce the visited-node limit before inserting seeds as well as traversal targets; report `visited_limit`. Track seed identities separately from visited identities so previously visited targets do not bypass the three-seed limit. | Fix here |
| MEDIUM | experiments/trusty-memory-relevance/metrics.py | 42 | Demand completion uses only demand IDs that already have selected support groups. An owner-plus-location request with only ownership support reports a completion rate of 1.0; unsupported or selection-dropped demands disappear from the denominator. | Pass parsed demands into scoring and report requested, supported, and completed counts separately. Use all requested supported-form demands for request completion; keep the current rate only if explicitly named as completion among selected support groups. | Fix here |

## Required Changes

1. Preserve candidate-only alias support in unknown-intent fallback, then verify whole-group fact and token limits.
2. Fix the visited-node guard and keep seed accounting independent of traversal visitation.
3. Add requested-demand accounting so partial packets remain distinguishable from fully satisfied requests.

## Evidence

### Alias support

Offending code, `relevance.py:74–75`:

```python
if overlap >= policy.unknown_overlap:
    groups.append(Support(e.id, (e.id,), demand_index))
```

Synthetic source metadata: `Nickname is_alias_for Alpha`; another source states `Alpha likes green ceramic cups.` Query: `Nickname green ceramic preferences`. The candidate set deliberately contains only the latter fact. Resolution consults the eligible index and resolves `Nickname`, but selection emits the endpoint without requiring the alias evidence. This violates the interface's explicit alias-support requirement and the candidate-only selector boundary for validating a complete path.

Actual helper-backed output:

```text
UNKNOWN_ALIAS {"demands": [{"intent": "unknown", "entities": ["Alpha"], "status": "unknown_intent", "aliases": ["test|alias|1|f"], "clause": "Nickname green ceramic preferences"}], "candidate_ids": ["test|fact|1|f"], "selected_ids": ["test|fact|1|f"], "supports": [{"endpoint": "test|fact|1|f", "members": ["test|fact|1|f"], "demand_index": 0}], "text": "## Project Context (from memory palace)\n\n### Facts\n- Alpha likes green ceramic cups.\n"}
```

### Visited-node limit

Offending code, `relation_support.py:61–66`:

```python
for seed in demand.entities:
    if seed not in visited and seed_count >= 3:
        truncated.add('seed_limit')
        continue
    seed_count += int(seed not in visited)
    visited.add(seed)
```

Synthetic graph: Alpha has 31 maintained-by neighbors; Beta and Gamma each have an ownership fact. Query: `Contact hours for Alpha; owner of Beta; owner of Gamma`.

```text
CAPS {"index_probes": 70, "examined_assertions": 33, "emitted_assertions": 2, "visited_nodes": 34, "seeds": 3}
```

### Partial-demand measurement

Offending code, `metrics.py:42–43`:

```python
demands = {s.demand_index for s in supports}
completed = {s.demand_index for s in supports if set(s.members) <= ids}
```

Synthetic query: `Who is the owner of Alpha and where is Alpha?`; only an ownership fact exists. Both demands parse, and the location failure is recorded, but the completion rate is 1.0.

```text
PARTIAL_DEMAND {"parsed_demands": 2, "supported_demands": 1, "completed_demands": 1, "complete_support_rate": 1.0, "rejections": [["Alpha", "no_complete_path"]]}
```

## Verification

Inspected supplied verification artifacts:

```text
/tmp/relevance-pytest-final.log: 12 passed in 3.27s
/tmp/relevance-mypy-final.log: Success: no issues found in 10 source files
```

Ran `/Users/masa/trusty-search-experiment/venv/bin/python /tmp/relevance-critic-probes.py` against custom synthetic data and the supplied actual Rust helper; output reproduced the three findings above.

Ran `/Users/masa/trusty-search-experiment/venv/bin/python /tmp/relevance-critic-integrity.py`:

```text
BASELINE_EQUIVALENCE PASS
SECOND_RECORD_ATOMIC_ROLLBACK PASS
CHECKPOINT_EVENT_REPLAY PASS
TEMPORAL_DUPLICATE_SEPARATION PASS
```

The baseline equivalence probe compared candidate IDs against the unchanged prior `bm25_graph` retriever. The rollback probe injected failure on the second event after the first record had successfully staged. The temporal probe confirmed different expiry and valid-from states do not collapse.

Source inspection also confirmed Task excludes ID/category/split, the selected-policy artifact is written before heldout gold parsing, complete emitted text is independently reconstructed before scoring, and invalid partial/old-policy derived records use explicit source fallback. No encoder construction or official experiment execution was performed. These observations are bounded to this experiment, not production readiness.

Parent continues with the findings and stage decision. The frozen sources remain unchanged; the next stage must retain this finding table if it proceeds under WARN.
