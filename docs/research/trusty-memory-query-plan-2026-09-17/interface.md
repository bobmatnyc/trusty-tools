# Memory query-plan interface and frozen grammar

Status: preimplementation experiment contract, 2026-09-17. Applies only to the new sibling experiment. The protocol is `docs/research/trusty-memory-query-plan-2026-09-17/protocol.md`; prior experiment source and fixtures remain immutable. This document supplies the fixture author with supported syntax, not answer annotations.

## Boundaries and files

Use `plan_contracts.py`, `plan_policy.py`, `plan_execute.py`, `plan_experiment.py`, `plan_evaluate.py`, and `test_query_plan.py`. Keep policy and executor individually below 500 nonblank/noncomment lines; target under 1000 new production lines total. Use plain immutable records and injected existing helper/index objects. No new service, repository, index, or DI framework.

Import existing `Task`, `CandidateSet`, `Selection`, `Support`, `consolidate`, `RelevanceIndex`, `DerivedStore`, `packet`, and independent evidence metrics. Use a single import bridge to put immutable relevance and graph directories on the import path. New modules must not shadow their module names or modify their globals.

## Typed boundaries

These signatures describe the boundary; they contain no implementation bodies. Index, evidence, packet, helper, and encoding types are imported from the existing experiment.

```python
from dataclasses import dataclass
from typing import Literal, Mapping

Arm = Literal['old_combined', 'defect_fixes', 'structured_plan']
Direction = Literal['out', 'in']
ParseStatus = Literal['ready', 'unsupported', 'unresolved_entity', 'ambiguous_entity']
ExecStatus = Literal['not_run', 'complete', 'no_evidence', 'partial', 'bounded']
Predicate = Literal['owned_by', 'maintained_by', 'located_at', 'access_code',
    'rationale', 'release_rule', 'release_prohibition', 'uses_channel',
    'depends_on', 'uses', 'escalates_to', 'contact_window', 'is_alias_for']

@dataclass(frozen=True)
class Span:
    start: int
    end: int
    text: str

@dataclass(frozen=True)
class EntityRef:
    span: Span
    targets: tuple[str, ...]
    alias_ids: tuple[str, ...]
    status: ParseStatus

@dataclass(frozen=True)
class Step:
    predicate: Predicate
    direction: Direction
    # Normalized literal qualifier; currently only "after HH:MM UTC".
    qualifier: str | None = None

@dataclass(frozen=True)
class Request:
    span: Span
    roots: tuple[EntityRef, ...]
    steps: tuple[Step, ...]
    excluded_targets: tuple[EntityRef, ...] = ()
    excluded_outputs: tuple[Predicate, ...] = ()
    enumerate_aliases: bool = False
    status: ParseStatus = 'ready'

@dataclass(frozen=True)
class Plan:
    requests: tuple[Request, ...]
    # Every unsupported substantive clause remains represented here.
    diagnostics: tuple[tuple[Span, str], ...]

@dataclass(frozen=True)
class Path:
    request_index: int
    root: str
    # Root followed by bound entity nodes; preserves each branch identity.
    bindings: tuple[str, ...]
    endpoint: str
    members: tuple[str, ...]

@dataclass(frozen=True)
class Execution:
    paths: tuple[Path, ...]
    per_request: tuple[ExecStatus, ...]
    counters: Mapping[str, int]
    reasons: tuple[tuple[str, str], ...]

@dataclass(frozen=True)
class RunResult:
    plan: Plan
    execution: Execution
    candidates: CandidateSet
    selection: Selection
    provenance: Mapping[str, tuple[str, ...]]

def parse_plan(task: Task, index: RelevanceIndex) -> Plan: ...
def execute_plan(plan: Plan, index: RelevanceIndex,
                 allowed: frozenset[str] | None = None) -> Execution: ...
def retrieve_shared(task: Task, plan: Plan, index: RelevanceIndex,
                    helper: RustHelper) -> CandidateSet: ...
def select_plan(plan: Plan, execution: Execution, candidates: CandidateSet,
                index: RelevanceIndex) -> tuple[Selection, Mapping[str, tuple[str, ...]]]: ...
def run_arm(task: Task, arm: Arm, index: RelevanceIndex,
            helper: RustHelper) -> RunResult: ...
```

Span offsets are Python string offsets into the original prompt, not UTF-8 evidence offsets. Preconditions: nonempty prompt, existing validated Task clocks, index built for this Task scope/time, valid slice bounds, and at most two relation steps. Invalid public inputs raise the inherited `IntegrityError`. Unsupported language returns diagnostics, not an exception. Every returned evidence ID belongs to the eligible index; every selected endpoint has a complete path. These invariants require violation tests.

## Frozen vocabulary and grammar

Case, ordinary punctuation, optional leading `please`, `give`, `show`, `list`, `identify`, `find`, `name`, `compare`, and articles do not change meaning. This is a bounded grammar, not unrestricted language understanding. Unrecognized positive questions remain in evaluation and normally return `unsupported`; they must not be discarded or reclassified as negative questions.

Relation phrases map one-to-one:

| Phrases | Predicate |
|---|---|
| owner, accountable owner, ownership, owned by | owned_by |
| maintainer, maintenance team, maintained by | maintained_by |
| location, site, address, located at | located_at |
| access code, entry code, door code | access_code |
| reason, rationale, original purpose | rationale |
| release rule, release prerequisite, release prerequisites, approval prerequisite | release_rule |
| release prohibition, explicit prohibition, prohibition on releasing | release_prohibition |
| channel, notification channel | uses_channel |
| dependency, dependencies, immediate dependencies, depends on | depends_on |
| asset, assets, used asset, used assets, uses | uses |
| escalation, escalation target, escalates to | escalates_to |
| contact window, availability, contact hours | contact_window |
| alias, alias meanings, interpretations, alternatives | is_alias_for |

The `release_prohibition` predicate is an additive curated assertion. It does not infer polarity from arbitrary prose or transform `release_rule` into a prohibition. The same sources and predicate values are visible to every arm. A query's optional `after HH:MM UTC` qualifier must match that normalized contiguous text in the endpoint object or claim; no arithmetic or temporal inference is implied. Other fact qualifiers are unsupported.

Accepted reference forms: a quoted name (`"Name"` or `'Name'`), `named Name`, or a maximal contiguous title-case name span. Names may contain letters, digits, internal hyphens, and spaces. The maximal span must resolve exactly, case-insensitively, against eligible names/aliases. Quoted and explicit `named` spans take priority. Consume a possessive suffix separately. Never shorten an unknown maximal mention to a known prefix. Sentence-leading command words and relation vocabulary are not name tokens. Lowercase unquoted multiword names and ambiguous boundaries are unsupported; no future/ineligible catalog is consulted.

Supported clause structures (`E` is a complete reference; `R` is a table phrase):

1. Direct: `R of E`, `E's R`, `where is E`, `who owns E`, `who maintains E`, `why was E created`.
2. Shared output: `R of E and F`; shared subject: `R and S of E`.
3. Independent requests: `R of E; S of F`, or `R of E and S of F`. Bind each output to its own reference. Do not cross-product them.
4. Inverse: `services maintained by E`, `services owned by E`, `services that depend on E`, or `incoming R of E`. These reverse the matching predicate, not every relation in the clause.
5. Composed: `R of the S of E`, including `R of both/all [immediate] dependencies of E` and `R of services maintained by E`. Execute the inner relation before the outer relation. Maximum two steps; intermediate facts require `object_entity` for outgoing traversal, while incoming traversal binds the fact subject. A plural branch means every matching bound node, not an arbitrarily selected node.
6. Alias enumeration: `list both/all alias meanings of E`, `list both/all interpretations of E`, or `what could E refer to`. Return eligible alias assertions, even when ambiguous. For enumeration, bind the literal alias subject as the root (`targets=(alias_name,)`, ready), rather than resolving it to its alternatives before traversal. `R of [both/all] alias meanings of E` is a two-step composition through `is_alias_for` and then R; retain each alias edge. Ordinary requests through an ambiguous alias abstain; a unique alias adds its assertion to every support path.
7. Output exclusion: append `, not R`, `; omit R`, or `; R would not answer this`. These suppress output R and do not become positive requests. They cannot negate an earlier different relation.
8. Target exclusion: append `excluding E` or `except E` to a plural/inverse/path request. Remove paths whose final bound entity matches E. An unresolved excluded name is diagnosed, not silently ignored. This is not exclusion of arbitrary claim text.
9. Presentation constraints: `once`, `without duplicates`, `do not pick a single referent`, `list both`, and `not the shorter similarly named [asset/annex]` are explicit non-relational modifiers. They never veto an otherwise positive request.

Split independent clauses at semicolons and sentence boundaries, preserving offsets. Treat supported exclusion/presentation clauses as modifiers of the preceding request. `and` inside shared-subject/output or entity lists binds only within that clause. A continuation with explicit `its R` may inherit exactly one resolved previous root; every other unstated antecedent is unsupported. Any new unresolved explicit name blocks inheritance. `without following dependencies` forbids a `depends_on` step; it does not suppress direct requests or alias provenance. Arbitrary `not`, `never`, hypothetical clauses, disjunction logic, relative comparisons, and three-step paths are unsupported rather than globally vetoed or guessed.

## Treatment semantics and execution bounds

`old_combined` calls immutable prior combined retrieval/intervention with overlap1 and cap4. Its native failures remain the control. Store neutral plan diagnostics for this arm; do not claim it implements the new parser.

`defect_fixes` and `structured_plan` share **joint plan-directed retrieval**: native claim BM25 top20 plus bounded paths from the structured plan, fused with RRF60. Missing derived records use the inherited source-BM25 fallback. Record the same candidate IDs for both arms. The control-to-repair difference bundles retrieval changes and repairs; it is not a pure selector ablation.

The repaired selector keeps a flat demand model. Separate owner/maintainer, honor complete references and explicit exclusions, keep positive requests with presentation negatives, and deduplicate before cap. For composed clauses it treats recognized relation outputs as requests on the explicit root, preserving the old flattening limitation. It does not consume the structured executor's Path objects or Support annotations. Inverse and alias behavior remain the old supported templates unless a direct defect repair explicitly requires otherwise. Do not silently install two-step variable binding in this arm.

The structured selector executes ordered steps, direction, exclusions, aliases and branch bindings. Pass candidate IDs as `allowed` for final selection so a selector cannot fetch hidden evidence. Unrestricted bounded execution supplies the shared retrieval lane; selection execution must remain candidate constrained. Do not charge candidate membership misses as examined assertions, but report them separately from posting entries examined.

Frozen bounds: three distinct roots, four requests, two steps per path (unique-alias provenance does not consume a relation step), 32 visited `(request,step,node)` bindings, 128 examined assertions, 32 emitted candidate semantic facts, four selected task semantic facts. Count posting probes independently. Cap exhaustion produces `bounded`, never `no_evidence`. Retain all omitted-request diagnostics; never truncate a fifth request invisibly. Deterministic ordering uses request order, existing fused candidate rank, path length, then evidence ID.

Canonicalize eligible candidates with inherited `consolidate` before charging selected fact slots. Map every path member/endpoint to its canonical ID. Equivalent assertions retain all eligible provenance members; conflicts and distinct intervals remain separate. Admit whole support groups when their union fits cap4. Shared edges count once. Conversion to inherited Support uses request index and root entity; bindings remain in Execution diagnostics. Existing packet packing enforces complete support at budgets128 and256; report budget-dropped paths separately from prepacket status. Standing evidence follows the inherited packer and is outside the task fact cap, inside token budget.

Per-request execution status: unsupported/unresolved/ambiguous requests produce `not_run`; ready with no complete eligible paths produces `no_evidence`; all discovered branches fulfilled produces `complete`; known branches with missing endpoints produce `partial`; any relevant execution/selection cap produces `bounded`. A request with one path and another missing branch is partial even if its endpoint evidence is correct. These statuses are diagnostics, never an independent completeness oracle.

## Independent evaluation and acceptance

Use existing evidence metrics with `demands=()`. Omit old parser-scored demand completion aggregates from the report. Candidate, selected, and final required-group coverage and all-required success come from independently authored gold. Count unsupported positive requests as positive failures, not abstention successes. Parser diagnostics may explain failure but cannot erase required groups.

Freeze all vocabulary, syntax, bounds, fixtures, and policy hashes before the official run. Hash every imported relevance and graph Python module plus the Rust helper binary/source, not only new policy files. Fixture authors should include supported structures, unsupported positive paraphrases, exact unknown longer names, scoped/time-ineligible references, inverse-plus-property paths, partial branches, independent relation/entity pairs, exclusion of one alternative, and duplicates preceding distinct required facts. Keep enough cases within four semantic facts to avoid testing only known capacity limits; label larger-capacity cases only in evaluator metadata. Retain old failures solely as development regressions. Test alias provenance, cap exhaustion, unsupported clauses, independent bindings, dedup conflicts, and mismatching/missing prohibition qualifiers with small separate fixtures.

Reuse deterministic maintenance unchanged. This experiment requires no production schema migration or full reindex; it makes no production compatibility claim beyond leaving production unchanged. Embeddings and model construction remain off.

## Premeasurement review amendment: explicit index context

A scope/time mismatch probe showed that the inherited index type does not retain its construction context. Add `plan_index.py` with an explicit `ScopedIndex(RelevanceIndex)` dataclass carrying `task_context: tuple[str, str, str]` for scope, as_of and knowledge_cutoff. `build_scoped_index(sources: tuple[Source, ...], task: Task, store: DerivedStore, helper: RustHelper) -> ScopedIndex` wraps the unchanged index builder. `validate_context(task: Task, index: RelevanceIndex) -> None` rejects an unscoped index or context mismatch with `IntegrityError`. Public task-taking entry points validate before parsing or retrieval; evaluator and synthetic tests use this constructor. Existing helper compatibility is retained through the subtype. No global context registry or production schema change. This amendment was made before official ranking and does not change grammar or fixture labels.
