# Verification and interpretation

The experiment is isolated from production. Previous experiment source, fixtures and results are retained unchanged. New code reuses the existing Rust BM25 and prompt formatter helper without changing or rebuilding production retrieval. Embeddings and answer generation are excluded.

## Fixture independence

The fixture author and policy designer shared the schema and predicate vocabulary only. The implementer was instructed not to inspect new query wording or gold while implementing the frozen policy. An independent fixture review ran before ranking. It identified an overgenerous acceptable-evidence label in incident-route queries; that label is corrected before freezing, without changing required facts or retrieval policy.

Heldout wording differs, but most scenario structures share tuning templates. The two heldout dependency-comparison questions introduce a second branch; they are only two of 48 heldout queries. This is an independently authored synthetic holdout with limited structural novelty, not broad natural-query validation. Curated typed predicates are available to the experiment; automatic extraction quality remains untested.

## Measurement boundaries

Finite source-snapshot, scope and clock projections are prepared outside timed queries. Warm query latency includes retrieval, selection, fusion, packing and tokenization, but excludes eligibility projection construction, scoring and startup. The reused native helper is a debug build; latency is not a release-daemon measurement. Native index reconstruction and bounded derived-record maintenance are reported separately. A bounded source batch does not establish bounded total CPU or memory.

Baseline graph lookup retains one seed and one entity hop; relation-aware lookup uses up to three seeds and two hops. That intervention measures the whole relation policy, not an isolated benefit of predicate indexing. Claim BM25 retrieves 20 claims, while source BM25 retrieves up to 20 sources and expands their facts; differences in candidate volume are part of this intervention and must be reported.

Identical titles intentionally provide little claim discrimination. Source BM25 retains full source bodies, including text from ineligible facts, as in the previous experiment; all emitted facts must pass current eligibility. Exact cleanup cannot merge distinct values, validity intervals, scopes or expiry states. Cleanup is never evidence refresh.

## Initial gates and pre-run review corrections

Initial focused tests passed (12 tests); strict mypy checked ten source modules successfully. The [independent critic](evidence/critic-initial.md) returned WARN with one HIGH and two MEDIUM findings before any official ranking: unknown-intent alias fallback could omit its alias support, additional seeds could exceed the visited-node cap, and completion counted only selected supported demands.

The corrections retain the frozen query vocabulary and gold. Support records gain an explicit entity anchor so scoring can distinguish each requested intent–entity binding. Requested completion is separate from retention of already-selected support, and gold required-group coverage remains the primary task-success measure. Negated and hypothetical demands are not positive requests; unresolved or unsupported positive requests remain visible in completion denominators.

Final corrected tests: **18 passed in 5.27s**; strict mypy: **ten source modules clean**. [Final critic](evidence/critic-final.md): APPROVE, with one subsequent MEDIUM duplicate-alias completion diagnostic corrected before ranking. Its [independent probe](evidence/alias-completion-probe.log) passes, and the final tests additionally distinguish exact duplicates from omitted alternative targets. [Security](evidence/security.md): zero HIGH/CRITICAL findings; guarded tests blocked Python network, encoder construction and official fixture/gold reads. The subsequent alias metric change affects no I/O or security boundary. Inherited LOW helper-timeout and dependency-lock observations remain documented.

Run 01 completed with all four tuning cells and six heldout arms. All eight frozen input/spec/policy hashes remained unchanged throughout review fixes and execution. No parameters or labels changed after heldout exposure. Original experiment source/input hashes were checked unchanged. Final logs are [pytest](evidence/pytest-alias.log), [mypy](evidence/mypy-alias.log), and [evaluation](evidence/evaluation-run-01.log). The [numerical audit](evidence/results-audit.md) independently recomputed scores and checked actual claims; its author also authored the fixture, so this is not independent dataset validation. The [artifact audit](evidence/artifact-audit.log) verifies frozen/evaluated hashes, compressed result readability and targeted credential patterns.

## Post-run implementation deviation

Source diagnosis after results identified a broader unknown-intent negation veto than the interface specified. `query_policy.py` applies the three-token window for recognized phrases, but its no-match branch rejects any clause containing `not`, `never` or `without`. The evaluated source was hashed before ranking; results and labels are unchanged. This contributes to false abstentions reported in the results and must not be described as an inherent limitation of deterministic retrieval. Correcting it and claiming improved performance requires fresh validation, not rerunning the exposed holdout as independent evidence.
