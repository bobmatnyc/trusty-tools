# Structured memory queries: bounded next experiment

Source review, 2026-09-17. Prior `experiments/trusty-memory-relevance` remains immutable. New work belongs in `experiments/trusty-memory-query-plan` and `docs/research/trusty-memory-query-plan-2026-09-17`. Embeddings off; no production or Rust changes.

## What fails and what can be reused

Old `query_policy.py` conflates ownership with maintenance in both `LEXICON` and `PREDICATES`, resolves only known entity substrings, and applies a word-window negation veto. Its `parse_intents` flattens every recognized intent across every resolved clause entity and truncates at three demands. Unsupported unknown clauses can pass through overlap selection. These mechanisms explain wrong predicate, prefix substitution, scope of negation, and composed-request failures; more synonyms alone do not resolve them.

`relation_support.py::relation_lookup` has direction handling only for reverse dependencies and hard-coded transitions for contact/channel/location/access. It has no general representation of a requested endpoint property after a dependency path. `relevance.py::select_claims` enforces a fact cap before `intervention` consolidates duplicates, so equivalent source assertions can consume slots needed by distinct evidence.

Reuse the resident helper, `relevance_index.py` eligibility projection and directed postings, `contracts.py::semantic_key/consolidate`, `relevance.py::packet`, prior complete-claim formatter and independent packet verifier. Preserve native claim BM25 and lexical candidate cap20. Reuse metrics for evidence accuracy, but **do not** use parser-produced demand counts as the only completeness oracle: a parser that misses a demand can score its own reduced plan as complete. Required gold groups remain independent.

Reuse maintenance unchanged as a regression/compatibility check. This round changes query interpretation, not index publication. No new index abstraction, embedder, service harness, or schema migration is required.

## Three principal arms

1. **old_combined**: exact prior combined policy, including its selected four-fact cap and unknown overlap1. Same new sources and budgets, imported unchanged.
2. **defect_fixes**: separate owner/maintainer predicates; preserve exact entity boundaries; distinguish output-exclusion wording from affirmative requests; consolidate equivalent assertions before applying cap. Keep old flat demand/traversal model. Explicitly list this bundled repair's scope; it is not a single-mechanism effect.
3. **structured_plan**: same candidates, eligibility and dedup-before-cap as repaired arm; replace flattened demands with bound directed relation expressions and explicit exclusions. Keep caps/budgets unchanged. Compare against repaired arm to isolate structured interpretation beyond repair.

Optional fourth **broad_candidates**: identical structured parser/selector with a larger predeclared lexical candidate cap. Use only to test candidate starvation, not as the main quality claim. If every required fact is already a candidate, skip this arm and report that fact. Avoid another tuning grid unless a bounded policy choice actually needs tuning.

## Minimal query-plan contract

Pure `parse(prompt, entity_catalog, relation_vocabulary) -> Plan`. No query IDs, categories, splits, gold, or source answers. A plan contains original text spans, requested outputs, entity bindings, ordered relation steps, exclusions and parse status. A relation step has a predicate set, direction (`out`/`in`), and binding variable. Literal endpoint properties are distinguished from entity-to-entity traversal.

Example abstract form: `seed(service) -> depends_on(out, dependency) -> release_rule(out, answer)`. Preserve each dependency binding; do not transform it into separate `dependency(service)` and `release_rule(service)` requests. Inverse maintenance is `maintained_by(in, service)` from a team seed. Ownership is `owned_by`, never an implicit synonym for maintenance. Multiple requested outputs share variables only where the syntax binds them; two entities with different predicates must not form an unintended Cartesian product.

Status vocabulary must distinguish:

- `unsupported`: the bounded grammar cannot express the request; no claim of absent knowledge.
- `unresolved_entity`: an explicit reference cannot bind to a known scoped entity.
- `ambiguous_entity`: several eligible entities fit and the user did not ask to list alternatives.
- `ready`: a supported plan with resolved bindings.
- `no_evidence`: execution found no complete eligible support path for a ready plan.
- `partial`: some requested outputs have complete support and others do not.
- `bounded`: execution stopped at a declared cap; absence is not exhaustive.

Recognition and execution statuses are separate. “No stored evidence” is not a synonym for “parser unsupported.” Return reason codes and source spans for plan decisions without inserting these diagnostics into the user's evidence packet.

## Entity, polarity and exclusion rules

Preserve exact mention spans before resolution. A known prefix inside an explicitly longer unknown asset name is not a valid substitute. Retain unresolved span information instead of falling back to the previous subject. For arbitrary unquoted names a heuristic cannot prove the intended boundary; fail with unresolved/unsupported rather than silently answering about another entity. Freeze the supported name grammar and test both quoted and unquoted forms.

Represent `request`, `exclude_output`, and `negative_relation` separately. “Give location, not owner” keeps the location request and excludes owner. “Do not choose one; list both alias meanings” is a positive ambiguity-enumeration request. “Without following dependencies” constrains traversal, not alias expansion. “Which policy forbids X?” requests a stored prohibition relation/value; an approval prerequisite cannot satisfy it. Unknown polarity/scope is unsupported, not an affirmative keyword match.

Use conservative antecedent bindings only for explicit supported continuation forms. A new unresolved named reference must not inherit the previous entity. Unknown clauses remain visible in diagnostics so partial success cannot hide unparsed requests.

## Execution and duplicate contracts

Execute directed postings with bounded seeds, visited bindings, relation depth, inspected assertions, emitted semantic facts, and prompt tokens. Return complete evidence paths per output. Record both posting probes and inspected assertions; indexed skipping of irrelevant predicates is different from postfiltering scanned edges.

Canonicalize eligible exact-equivalent assertions before charging semantic fact slots. Preserve all provenance members and never merge conflicts, intervals, scopes or expired/current states. Map support paths to canonical IDs before cap decisions. Pack complete support groups; either select a supported endpoint with its connecting evidence or report why the group could not fit. Shared path facts count once; all their provenance remains available.

## Evaluation constraints

Fresh heldout must include new binding structures: different predicates on two named entities, an inverse relation followed by endpoint property, two alternatives with one excluded, supported positive request with negative presentation constraints, an unresolved longer entity alongside a known shorter one, and partial composed requests. Separate parser accuracy, binding/path accuracy, candidate coverage, selected coverage and packet coverage. Independently authored gold may annotate expected plan structure evaluator-side; it must never reach parser execution.

Report coverage/abstention tradeoffs and the90%/90% target unchanged. Keep positive unknown-language cases so always abstaining on unknown forms cannot masquerade as success. No claim that deterministic grammar understands unrestricted language. Counterexamples from the exposed prior heldout may be regression tests, never fresh validation. Freeze grammar/policy and fixture hashes before ranking.

Parent continues interface/fixture assignments. This is a design recommendation supported by current source inspection; no implementation or benchmark claim.
