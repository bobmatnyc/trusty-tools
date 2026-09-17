# Query-plan experiment security review

Review date: 2026-09-17. Worktree: `/Users/masa/trusty-search-experiment/worktree`.

## Summary and gate

**Final zero HIGH/CRITICAL gate: PASS.** One new LOW parser-input issue was reproduced, fixed before measurement, and independently verified resolved. The inherited helper-response timeout and transitive dependency-lock LOW observations remain unchanged.

Scope: seven production Python modules, independent test module, README, interface and protocol for `experiments/trusty-memory-query-plan`. Existing relevance/graph infrastructure is reused and was reviewed earlier; no broad dependency re-audit was performed. This review did not edit repository files, run Git operations, install dependencies, download models, read official fixture/gold data, or execute official rankings. Review artifacts and guard scripts are in `/tmp`.

## Finding and remediation

**LOW, RESOLVED — literal input collided with internal reference markers.** Before the fix, `plan_policy.py` accepted literal `@N` text through its expression grammar, then indexed the generated references without validating the marker's origin or range. With one private synthetic source, both inputs below raised an uncaught exception:

```text
'owner of @0' IndexError tuple index out of range
'owner of Alpha excluding @99' IndexError tuple index out of range
```

Impact was an aborted local request/experiment; no code execution or scope escape was observed. The final `plan_policy.py:141` rejects literal reserved markers before reference substitution, emits `reserved_reference_marker`, and clears antecedent state. Both cases in `test_query_plan.py:211` now assert unsupported status and zero executable paths. The complete guarded suite passed after this fix. No remaining remediation is required for this finding.

## Reviewed controls

- `plan_index.py:31` rejects unscoped index instances and any scope/as-of/cutoff mismatch. `parse_plan`, `retrieve_shared`, `run_arm` and `measured_case` validate context before their task operations. Guarded tests exercise missing context, foreign scope and both clock mismatches. The context property is an internal consistency check, not user authentication or protection against malicious same-process code mutating Python objects.
- The import bridge derives both prior directories from `__file__`; it does not select imports from prompt text. No new production eval, exec, subprocess, pickle, YAML deserialization, network client or encoder-construction call was found. Existing Python environment/module resolution and the selected helper binary remain trusted.
- `plan_evaluate.py:51` resolves manifest paths, rejects duplicate resolved paths and paths outside the repository except the explicitly selected helper. It checks file hashes and requires coverage of the code, specifications, fixtures and helper before ranking. The manifest and repository remain trusted; this does not authenticate a maliciously replaced manifest or eliminate same-user file-replacement races.
- `plan_evaluate.py:116` requires a nonexistent output directory. It reuses `evaluate.save` from the relevance experiment, which serializes JSON with `allow_nan=False` and writes using exclusive `xb` creation. Result names are fixed by code, not generated from source text.
- Official gold is read only by the evaluator and passed to scoring after candidate retrieval and packet construction. This experiment has fixed arms and no parameter-selection grid. The evaluator saves fixed policy provenance before iterating tuning/heldout evaluation. Source review establishes this ordering; official evaluation was not executed.
- Plans contain immutable data records, not executable expressions. Candidate-constrained execution checks supporting aliases and relation evidence against allowed IDs. Selection admits complete canonical support groups under the four-fact cap. Scope, temporal eligibility, source provenance, packet integrity and maintenance are inherited controls.
- Credential-pattern scan over the eight Python files, README, interface and protocol returned `SCOPED_FILES 11`, `CREDENTIAL_PATTERN_FILES []`. AST call inspection returned `ENCODER_DYNAMIC_EXEC_CALLS []`. Official fixtures and repository history were excluded from this scan.

All unqualified file references above are within `experiments/trusty-memory-query-plan/`.

## Verification Results

No repository source changed during review. The initial frozen contract suite completed under `/tmp/queryplan-security-guard.py` with observed **`EXIT=0`**. It runs all independent `test_query_plan.py` tests with Python bytecode and pytest cache writes disabled and plugin autoload disabled. It blocks official sources/queries/gold reads across all three experiment directories, Python socket connections/DNS, `LocalEncoder` initialization, and ONNX `InferenceSession` construction. A caught forbidden operation still fails the overall guard.

Initial log: `/tmp/queryplan-security-guard.log`. The targeted private-source parser probes above ran separately. The parent reports 19 normal tests passing, seven modules clean under mypy, and a regenerated 38-input manifest after the fix; those are parent observations. The independent test suite includes a tiny synthetic measured-case contract, but it neither reads official gold nor executes the experiment's official ranking runner.

Final post-fix guard command: `PYTHONDONTWRITEBYTECODE=1 PYTEST_DISABLE_PLUGIN_AUTOLOAD=1 /Users/masa/trusty-search-experiment/venv/bin/python /tmp/queryplan-security-guard.py`. Observed result: **`EXIT=0`**. Final raw log: `/tmp/queryplan-security-guard-final.log`. Final source checks confirm the marker rejection and both regression cases. Status: **VERIFIED within the stated local experiment scope**.

## Limits and OWASP coverage

No new dependency was added; the README reuses prior pins. Current CVEs, transitive licenses, obsolescence and native/Rust library internals were not re-audited. The inherited LOW remedies remain a bounded helper IPC deadline with child cleanup and a complete dependency lock with artifact hashes if operational reuse requires them.

Python audit and constructor guards do not instrument native-library or Rust-child syscalls; this is not OS-level egress proof. Manifests, sources, checkpoints, indexes, helper paths and Python modules are trusted local inputs. Input size is not bounded as it would need to be for a hostile network service. The context wrapper is not a multi-tenant authorization layer. Provenance cannot prove natural-language instruction safety for a future answering model; this experiment constructs evidence packets without an answering agent.

OWASP coverage includes experiment-level access/scope checks, injection and deserialization review, output/manifest integrity and dependency-change inspection. Vulnerable-component coverage remains limited as stated. No web authentication, CSRF, XSS, session or SSRF endpoint is exposed by this local experiment. No production-deployment clearance is implied.

## Handoff

Parent continues evaluation. The reserved-marker LOW fix and final guarded verification are complete. No HIGH/CRITICAL remediation was identified. Retain the inherited LOW observations and the trusted-input, dependency-advisory and native-egress limitations.
