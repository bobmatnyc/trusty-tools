## Verdict: WARN

## Findings

Paths below are relative to `/Users/masa/trusty-search-experiment/worktree`. Review used the frozen research/interface/protocol documents, implementation, supplied test outputs, and separate synthetic probes. No official sources, queries, gold, or rankings were read or run.

| Severity | File | Line | Issue | Fix | Disposition |
|----------|------|------|-------|-----|-------------|
| HIGH | experiments/trusty-memory-query-plan/plan_policy.py | 149 | A semicolon output exclusion attaches only to the last generated Request, so an excluded output from the preceding shared-output clause is still returned. | Track the preceding syntactic clause's generated requests and apply its output exclusion across that group. Add packet/selection assertions for both supported semicolon exclusion forms. | Fix here |
| HIGH | experiments/trusty-memory-query-plan/plan_experiment.py | 60 | The documented scoped-index precondition is not checked. A foreign-scope Task paired with a test-scope index returns test-scope evidence, instead of the required IntegrityError. | Validate Task scope/clocks against the index construction context before parsing/retrieval in public task-taking entry points; reject mismatches with IntegrityError. Add scope and time mismatch violation tests. | Fix here |
| MEDIUM | experiments/trusty-memory-query-plan/plan_execute.py | 66 | Candidate membership misses can hide a known branch while per_request reports complete. Independent coverage remains valid, but execution diagnostics misidentify candidate loss as complete execution. | Track missing relevant, unexcluded branches when a posting fails candidate membership. Return partial when another branch succeeds, and retain candidate-loss reasons. | Fix here |

## Reproduction and impact

### 1. Exclusion attaches to the wrong generated request

Offending code, `plan_policy.py:149`:

```python
requests[-1] = replace(requests[-1], excluded_outputs=(*requests[-1].excluded_outputs, output.predicate))
```

The interface supports shared subjects and `; omit R` / `; R would not answer this` (`interface.md:129,134,138`). A shared-subject clause emits multiple Request objects, but the modifier changes only the final one. With separate ownership and location assertions for Alpha:

```text
'owner and location of Alpha; omit owner' => [('ready', ['owned_by'], ()), ('ready', ['located_at'], ('owned_by',))] [('Alpha', 'owned_by'), ('Alpha', 'located_at')]
'owner and location of Alpha; owner would not answer this' => [('ready', ['owned_by'], ()), ('ready', ['located_at'], ('owned_by',))] [('Alpha', 'owned_by'), ('Alpha', 'located_at')]
```

Both selections contain the explicitly excluded ownership fact. This affects both repaired arms because they share parsing.

### 2. Public boundary accepts an index for another task scope

Offending code, `plan_experiment.py:60-68`:

```python
"""Pre: known arm and scoped index. Post: evidence is eligible and fully supported."""
if arm not in ARMS:
    raise IntegrityError('unknown arm')
# ...
plan = parse_plan(task, index)
```

`parse_plan` likewise checks only the empty prompt at `plan_policy.py:128`; neither boundary checks the supplied Task against the index. The interface explicitly requires invalid public inputs to raise IntegrityError (`interface.md:98`). The synthetic call is:

```python
run_arm(replace(task(), scope='foreign'), 'structured_plan', index_for_scope_test, helper)
```

Observed output:

```text
WRONG_SCOPE => [('test', 'test|a|1|f')]
```

The API returns evidence from `test` for a `foreign` task. This finding is HIGH because it concerns documented invalid-input enforcement in an offline experiment. The official evaluator constructs indexes from task keys and independently checks packet eligibility; this review did not demonstrate an official-run leak or a production vulnerability. Enforce the public contract before a caller can consume RunResult directly.

### 3. A missing candidate branch reports complete

Offending code, `plan_execute.py:66-68`:

```python
if allowed is not None and evidence.id not in allowed:
    counters['candidate_membership_misses'] += 1
    continue
```

Synthetic eligible index: Alpha depends on First and Second; First is located in Rome; Second is located in Paris. Execute `location of dependencies of Alpha` with `allowed` containing only Alpha→First and First→Rome. The Second branch exists in the eligible index, but its connecting assertion is absent from candidates.

Observed output:

```text
candidate-lost branch => ('complete',) {'index_probes': 2, 'examined_assertions': 2, 'candidate_membership_misses': 1, 'visited_bindings': 2, 'emitted_semantic_facts': 2, 'seeds': 1} [(('Alpha', 'First'), 'test|c|1|f')]
```

The interface requires partial when known branches lack complete support (`interface.md:154`). The independent gold metrics can still reveal missing evidence, but the status used to explain that failure is incorrect.

## Required Changes

1. Correct exclusion scope and add selection-level regressions for shared-subject clauses.
2. Enforce the public Task/index compatibility precondition and add negative contract tests.
3. Correct candidate-loss status reporting with a two-branch regression.

## Verification Results

- Supplied pytest output: `14 passed in 3.35s`.
- Supplied mypy output: `Success: no issues found in 6 source files`.
- Independent suite: `/Users/masa/trusty-search-experiment/venv/bin/python -m pytest -q experiments/trusty-memory-query-plan/test_query_plan.py > /tmp/queryplan-critic-pytest.log 2>&1`; observed `EXIT=0`.
- Reproductions: `/Users/masa/trusty-search-experiment/venv/bin/python /tmp/queryplan-critic-probes.py` and `/Users/masa/trusty-search-experiment/venv/bin/python /tmp/queryplan-critic-boundary.py`; both exited 0 and printed the observations above.
- No source, Git, or ticket edits. Review and tiny probe files were written only under `/tmp`.

## Notes

`measured_case` passes empty demands to inherited metrics and removes parser-derived demand aggregates. Required-group coverage remains gold-derived. The manifest enumerates the new policy, all imported relevance/graph Python modules, specifications, fixture files, and helper source/binary. These source checks support evaluator structure; official fixture validity, official scores, and runtime adoption remain unverified by design.

Parent continues with the findings and disposition table. The implementation needs the repairs above before this review can be considered resolved.
