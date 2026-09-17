# Deterministic relevance experiment interface

Status: design-only handoff, 2026-09-17. Parent protocol governs:
`docs/research/trusty-memory-relevance-2026-09-17/protocol.md`.
Research: `/tmp/relevance-research.md`. No previous experiment file changes.

## Reuse and scope

Add the absolute previous experiment directory to `sys.path` explicitly, then
reuse `records.Source/Fact/Query/Evidence/Event`, eligibility, `RustHelper`,
`Retrieval`, `Packet`, the complete-claim packer and independent packet checker.
Resolve the path relative to the new script, not the current working directory.
Record hashes of imported files. Never import the previous evaluator or gold into
retrieval code. Do not construct `LocalEncoder` or call `build_source_index`.
An existing `Projection` may carry an empty `(0,384)` array solely to satisfy its
unchanged container shape; there is no dense lane or synthetic vector search.
Use the unchanged resident Rust helper for `load/search/format/classify`.

The new code defines a small intent/selection module, embedding-free index and
maintenance module, and evaluator. No framework, production source changes,
daemon, hosted calls, live memory access, or new dependency is required.

## Schema and leakage boundary

Inputs keep prior `graph-prompt-v1` JSON schema. Sources and facts use
`source_id`/`fact_id`; evidence identity is
`scope|source_id|revision|fact_id`. All timestamps are exactly
`YYYY-MM-DDTHH:MM:SSZ`. Reject other ISO spellings to make reused lexical timestamp
comparison sound. Source events replace complete records at increasing revisions;
tombstones have empty body/facts. Every claim matches its complete UTF-8 byte span.

Every query has `entity_hint: null`. Category, split, scenario routing and query ID
stay in the evaluator. Retrieval receives only `Task(prompt, scope, as_of,
knowledge_cutoff)` plus its prebuilt eligible index. Index construction may use
scenario to select the source snapshot. It must not expose that field to intent
recognition or ranking. Gold remains evaluator-only. Heldout gold cannot be parsed
until the selected-policy file has been written. Hash this interface, protocol,
query-policy constants and fixtures before the first ranking call.

Fixed fact predicates agreed with the independent fixture author:
`maintained_by`, `owned_by`, `located_at`, `access_code`, `rationale`,
`release_rule`, `uses_channel`, `depends_on`, `uses`, `is_alias_for`, `is_fact`,
`has_convention`, `escalates_to`, `contact_window`.
No fixture answer text becomes a policy synonym. Unknown predicates remain source
facts; they are not silently reclassified as a known relation.

## Frozen intent lexicon

Matching uses casefolded Unicode word tokens; listed multiword phrases are exact
contiguous token phrases. Do not stem or add synonyms after viewing results.
Longest phrase wins only for overlapping matches of the same intent; collect
distinct intents rather than stopping at the first match. Maximum three demands.

| Intent | Literal query phrases | Accepted predicate(s) |
|---|---|---|
| owner | owner, owned by, ownership, maintains, maintainer, maintained by, responsible for | owned_by, maintained_by |
| location | where, location, located, address, floor | located_at |
| access | access code, entry code, door code, passcode, combination | access_code |
| reason | why, reason, rationale, because | rationale |
| release | release, deployment, deploy, approval, rollout | release_rule |
| channel | channel, notify, notification, message, messaging | uses_channel |
| dependency | depends on, dependency, dependencies, relies on, requires | depends_on, uses |
| reverse_dependency | depends on this, depend on this, dependent, dependents, used by, relies on this | depends_on, uses; incoming |
| escalation | escalate, escalation, escalates to, backup contact | escalates_to |
| contact | contact, contact window, availability, hours, reachable, reach | contact_window |
| alias | alias, aliases, alternatives, could refer to, could mean, stands for | is_alias_for |

Phrase overlaps resolve `reverse_dependency` over `dependency`, and `escalation`
over the word `contact` inside `backup contact`. This table is lexical recognition,
not general language understanding. Unknown supported phrasing is a required test
stratum and its miss rate is reported.

Each demand applies to exact entity mentions in its clause. Clause boundaries are
semicolon, sentence terminator, and the token `but`; commas and `and` retain a
shared entity context. When a clause has no entity, it inherits the nearest prior
clause's unambiguous entities. If two entities and two intents share a clause,
apply both demands to both entities and report this coarse interpretation; do not
invent grammatical bindings. Explicit negation markers `not`, `never`, `without`
within three tokens before a matched phrase suppress that demand. A clause with
`hypothetical`, `suppose`, or `what if` is unsupported by this selector. Preserve
other supported clauses. Suppressed/unsupported demands cannot use the unknown
lexical fallback. These finite rules and limitations are part of the experiment.

## Entity resolution and relation support

Entity names and aliases are source metadata, scoped to the eligible projection.
Match exact boundaries, longest spans first; ignore shorter overlapping names.
For equal longest spans, retain all possibilities and mark ambiguity. An ambiguous
alias blocks ordinary demands anchored to that occurrence; it must not resolve
through a shorter prefix. Alias-alternative requests may return all corresponding
alias assertions without selecting a target. Multiple nonoverlapping unambiguous
entities are allowed, ordered by query position then exact name, capped at three.

The fixed relation plan admits direct matching facts on a seed, outgoing edges
for ordinary relations, and incoming edges for reverse dependencies. For contact
and channel demands only, it also admits support paths of at most two entity
edges from these templates: `maintained_by|owned_by` followed optionally by
`escalates_to`, or direct `escalates_to`; endpoint property is `contact_window`
or `uses_channel` respectively. For location/access demands, one optional `uses`
edge may lead to the corresponding property. Alias resolution preserves its
supporting alias assertion. Literal endpoint properties consume no entity hop.
No arbitrary path search or question-specific templates are allowed.

Return complete supporting paths as groups. A relation assertion whose object is
the requested answer is itself complete support. A property on a reached entity
requires its connecting assertions. Direct demands never admit unrelated
properties just because they share an entity. Selector-only may validate paths
only from its existing candidate facts; it cannot retrieve missing edges. The
relation-graph lane may retrieve the bounded supporting groups from its index.

Relation graph caps: three seeds, two entity hops, 32 visited nodes, 128 examined
assertions, 32 emitted assertions. Count index probes separately from examined
assertions. Direction/predicate postings may avoid unrelated adjacency reads.
Stable seed/path-length/evidence-ID order controls ties. Cycles and repeated paths
cannot consume unbounded work. Report every truncation condition. These fixed
caps differ from the old one-seed/one-hop baseline; the intervention is the whole
relation policy, not isolated proof about predicate indexing alone.

## Six arms and selector grid

All use identical source snapshots, finite clock projections, standing prelude,
RRF constant 60, complete-claim formatter, 128/256 token budgets, and safety filters.

| Arm | Candidate changes | Postfusion changes |
|---|---|---|
| baseline | old source BM25 (20 sources) + old generic graph (1 seed/1 hop) | none |
| selector | none | demand relevance and explicit abstention |
| relation_graph | replace graph lane with fixed relation policy | none |
| claim_index | BM25 documents are claims, limit 20 claims; old graph unchanged | none |
| cleanup | none | exact semantic duplicate collapse |
| combined | claim BM25 + relation graph | selector then duplicate collapse |

Source BM25 retains `source.title + '\n' + source.body`, including text from
ineligible claims, as in the old baseline. Only emitted eligible evidence is
permitted. Claim BM25 uses `source.title + '\n' + complete claim`. Repeated title
influence and candidate-work difference are disclosed; no extra title or subject
weight tuning. No seventh arm is required.

Selector grid, in fixed tie order:
`(unknown_overlap=1,max_task_facts=4)`, `(1,8)`, `(2,4)`, `(2,8)`.
Known demands need eligible relation/path support. Unknown intent permits only
facts directly attached to an unambiguous mentioned entity and at least the
configured count of distinct query/claim content-token overlaps after removing
the entity's words and this fixed stoplist:
`a an and are as at be by can do for from have how i in is it me of on or please
the this to was what when which who with would you`.
Unknown queries without an unambiguous entity abstain. Known but unsupported
demands abstain rather than using unrelated lexical facts. Unknown fallback is
casefolded whole-word overlap, not a calibrated BM25 probability.

The fact cap applies to task assertions, including connecting support. Whole
support groups must fit the fact cap. Packing must not emit a detached target if
its support cannot fit the token budget: remove that group's endpoint and record
`incomplete_path_budget`, retaining other independent groups. This guard applies
only when the intervention creates/validates path groups; unchanged baseline
packing remains unchanged. Multiple demands may yield a partial supported packet;
report demand completion separately from total abstention.

Choose only selector settings by the parent objective. Freeze the relation policy,
claim text, duplicate key and all caps now. Combined uses the selected selector
unchanged; no additional tuning or category-specific overrides.

## Types and signatures

Use immutable typed records; no new abstract hierarchy is required. Names below
are public experiment boundaries, with no implementation bodies prescribed.

```python
@dataclass(frozen=True)
class Task:
    prompt: str
    scope: str
    as_of: str
    knowledge_cutoff: str

@dataclass(frozen=True)
class SelectorPolicy:
    unknown_overlap: int
    max_task_facts: int

@dataclass(frozen=True)
class Demand:
    intent: str
    entities: tuple[str, ...]
    status: str

@dataclass(frozen=True)
class Support:
    endpoint: str
    members: tuple[str, ...]
    demand_index: int

@dataclass(frozen=True)
class Selection:
    evidence: tuple[Evidence, ...]
    supports: tuple[Support, ...]
    rejections: tuple[tuple[str, str], ...]

def parse_intents(task: Task, index: RelevanceIndex) -> tuple[Demand, ...]: ...
def relation_lookup(task: Task, demands: tuple[Demand, ...], index: RelevanceIndex) -> CandidateSet: ...
def select_claims(task: Task, demands: tuple[Demand, ...], candidates: CandidateSet, policy: SelectorPolicy) -> Selection: ...
def semantic_key(evidence: Evidence) -> tuple[str | bool | None, ...]: ...
def consolidate(evidence: tuple[Evidence, ...]) -> Consolidated: ...
def build_index(sources: tuple[Source, ...], task: Task, store: DerivedStore, helper: RustHelper) -> RelevanceIndex: ...
def retrieve(task: Task, arm: str, index: RelevanceIndex, helper: RustHelper, policy: SelectorPolicy) -> CandidateSet: ...
def maintain(store: DerivedStore, sources: tuple[Source, ...], events: tuple[Event, ...], batch_limit: int) -> Maintenance: ...
def packet(task: Task, selection: Selection, index: RelevanceIndex, budget: int, helper: RustHelper, encoding: tiktoken.Encoding) -> Packet: ...
```

`CandidateSet`: ordered evidence, complete support groups where available, per-stage
timings and bounded-work counters. `Consolidated`: representatives plus mapping
representative evidence ID to every member evidence ID. `RelevanceIndex`: eligible
evidence/by-source mapping, old generic projection, scoped entity/alias index,
directed predicate postings, separate native source/claim projection IDs, standing
facts and generation/hash. Projection IDs include source-generation and policy
version, scope, as-of and cutoff. Do not reuse the old clock-only key.
`DerivedStore`: per-source revision/digest/policy, claim documents, directed
postings and aliases, source provenance, sequence watermark, tombstone revisions,
pending source IDs. `Maintenance`: changed/removed/pending counts, examined source
and derived record counts, cursor, source/derived hashes and elapsed time.

Reuse `FixtureError`, `ProtocolError`, `IntegrityError`; invalid policy ranges and
unknown arm are explicit errors. Missing derived coverage is an explicit recorded
fallback, not an error or silent empty result. Rejection reasons include
`unknown_intent`, `unresolved_entity`, `ambiguous_entity`, `negated_demand`,
`hypothetical`, `wrong_relation`, `no_complete_path`, `below_threshold`,
`fact_limit`, `incomplete_path_budget`, and `duplicate_assertion`.

## Exact cleanup and scoring

Semantic key is the exact tuple `(scope, subject, predicate, object, object_entity,
valid_from, valid_to, source.expires_at, standing)`. No case folding, paraphrase
matching or conflicting-value merge. Only already-eligible evidence is collapsed;
retain all provenance and original timestamps. Representative is the lowest
evidence ID; its claim/span must remain unchanged. Cleanup preserves the earliest
rank occupied by a member. Reads do not write verification timestamps.

Precision numerator is the number of unique acceptable semantic task assertions;
denominator is all emitted task assertions, including duplicates. Required groups
count once. A representative is acceptable if an eligible member of its exact
semantic class is acceptable; retain that mapping for independent verification.
Neither cleanup nor scoring may transfer acceptability across different validity
states. Redundancy is emitted assertion count minus unique semantic count.
Report removed duplicate tokens directly using same-formatter packet difference;
do not estimate savings from character counts. All other metrics and policy-choice
rules are exactly the parent protocol. Candidate, postselection and final-packet
coverage are separate to locate losses.

## Bounded maintenance and compatibility gates

A maintenance call publishes derived records for at most `batch_limit` source
IDs. A changed source invalidates all prior revision-derived records; a tombstone
retains its revision watermark. Replayed event sequence is a no-op; conflicting
replay or revision regression fails. Derived records identify their policy version
and source digest. Partial/missing/old-policy records must never suppress native
source retrieval. A claim-index arm can add source-BM25 candidates for precisely
the missing sources, recording fallback source count and resulting candidate work.
At full coverage the intended claim lane alone operates.

Atomic publication must expose either a coherent old generation or the new one;
an injected failed batch leaves cursor and logical state unchanged. Persistent
checkpoint roundtrip plus replay produces the same semantic hash. Source-count
limits do not imply CPU/RAM limits: report copies/scans and native index rebuilds.
Finite scope/time native projections may be rebuilt outside query timing, with
their costs disclosed. This is prototype compatibility, not production migration.

Focused verification: policy/gold isolation and no encoder construction; longest
name/ambiguous alias; up-to-three demands plus negation and supported rationale;
incoming and two-hop paths/cycles/caps; unknown threshold and complete-group token
packing; equivalent duplicates versus conflicts and provenance; legacy/mixed/full
derived coverage fallback; replacement/deletion/no-op/resume/failure rollback;
clean-build versus incremental semantic equality; metadata clocks unchanged;
packet checker catches unmatched claims, stale revisions and over-budget output.
Test query metadata perturbation: same task with different ID/category/split must
produce the same candidates and packet. Timing and evaluator labels are excluded
from deterministic equality.
