# Resident graph prompt enrichment: experimental interface

Status: experimental contract, 2026-09-17. Owner: parent experiment agent.
This artifact is a handoff contract, not a numbered production specification.
Research: `/tmp/graph-prompt-research.md`. Existing source references are relative
to `/Users/masa/trusty-search-experiment/worktree`.

## Question and scope

Measure whether resident graph lookup adds useful factual evidence to a fixed-size
prompt, and whether it supplies that evidence faster than lexical or dense search.
The measurement unit is a completed prompt packet, not a top-k source list.
No LLM answers are generated. Report factual evidence coverage rather than answer
accuracy. Synthetic explicit relationships measure curated-graph potential; they
do not establish automatic relation-extraction quality on real memory.

Production sources, live palaces, daemons, configuration, default ranking, and
existing indexes remain unchanged. No hosted calls or model downloads occur.
Only the experimental examples, synthetic fixtures, runner, and evidence change.
Keep the implementation small: one resident Rust example, approximately 800 lines
of Python split by responsibility, and focused tests. Inject helper/model paths;
no service container, general plugin framework, or extra database abstraction.

## Frozen files and schemas

JSON objects reject unknown keys. Identifiers are nonempty strings; timestamps
are UTC RFC3339 strings with explicit `Z`. All arrays have stable input order.
Strings are Unicode; span offsets are UTF-8 byte offsets, end exclusive.

`sources.json` has `version: "graph-prompt-v1"`, `sources`, and `events`.

| Record | Required fields and types |
|---|---|
| Source | `source_id: str`, `scope: str`, `revision: int >= 1`, `title: str`, `body: str`, `observed_at: timestamp`, `verified_at: timestamp|null`, `expires_at: timestamp|null`, `deleted: bool`, `facts: list[Fact]` |
| Fact | `fact_id: str`, `subject: str`, `predicate: str`, `object: str`, `object_entity: str|null`, `claim: str`, `start_byte: int`, `end_byte: int`, `valid_from: timestamp`, `valid_to: timestamp|null`, `single_value_slot: str|null`, `standing: bool` |
| Event | `sequence: int`, `source: Source` |

Fact IDs identify assertions, not relevance labels. A fact belongs to exactly one
source revision; `(scope, source_id, revision, fact_id)` is its evidence identity.
The canonical string is `scope|source_id|revision|fact_id`, decimal revision and
no pipe characters inside identifiers. Gold uses these exact strings.
Every claim is an exact, complete substring of its source body at the supplied
span. Source prose contains every factual triple, alias, and relationship exposed
to any retriever. All treatments receive these same records. Duplicate fact IDs,
nonmatching spans, invalid intervals, and non-increasing revisions fail loading.
An event replaces the complete source with a higher revision. A tombstone has
`deleted: true`, `facts: []`, and `body: ""`; empty tombstone bodies are valid.
Graph edges derive only from explicit facts with `object_entity`; the object
label and entity identity must be declared in fixture metadata, never inferred
from gold. Literal facts attach to their subject. Aliases are explicit
`is_alias_for` facts; ambiguity remains represented rather than silently resolved.

`queries.json` contains `queries: list[Query]` and no labels.

`Query`: `id: str`, `split: "tune"|"heldout"`, `scenario: "initial"|"updated"`,
`category: str`, `prompt: str`, `scope: str`, `as_of: timestamp`,
`knowledge_cutoff: timestamp`, `entity_hint: str|null`.
An entity hint is a user-visible identifier included in the task, never a gold
answer entity. Its text is appended to lexical and dense query text alike.

`gold.json` is evaluator-only. Each row has `query_id: str`,
`required: list[list[str]]`, `acceptable: list[str]`, `forbidden: list[str]`,
`expected_empty: bool`. Inner required lists are acceptable alternate evidence
for one required fact; coverage counts a group once. IDs include scope/source
revision to distinguish changed assertions. An empty expected set is an explicit
negative query, not missing annotation. Gold cannot enter the Rust process,
embedding/index construction, graph seed selection, ranking, or packing.

`manifest.sha256` freezes these files and this accepted protocol before tuning.
Fixture validation rejects entity/alias overlap between tune and heldout.
Predicate vocabulary and task category overlap are allowed and documented.
Create genuinely new heldout entities and prompts; prior experiment examples
cannot serve as unseen validation. Do not revise gold after viewing results.

## Temporal and scope invariants

An eligible fact has the requested scope, a live source, observation at or before
the knowledge cutoff, unexpired source TTL, and `valid_from <= as_of < valid_to`
(null upper bound means open). A single-value slot selects the newest eligible
assertion by valid-from, observed-at, revision, and stable ID. Conflicting exact
ties are retained as ambiguity; they are not silently resolved into truth.
Every lane uses the same eligibility projection before selection. History is
evaluated from retained assertions, not a current-only graph. Reads, reformatting,
dream indexing, and cache publication never advance factual timestamps.
Projection/index construction occurs before query measurement. Query-time
eligibility filters candidate evidence; it must not rebuild the whole index.
If separate resident projections are built for the finite fixture clocks, report
that limitation and their complete construction/memory cost.

## Resident Rust helper protocol

Use `trusty_common::bm25::BM25Index` and
`trusty_memory::prompt_facts::{build_prompt_context, is_hot_predicate}`.
Current graph controls use
`trusty_common::memory_core::store::kg::{KnowledgeGraph, ExpandDirection}`.
One process serves JSONL stdin/stdout for the whole experiment. No process spawn,
index hydration, corpus embedding, or full state serialization inside a measured
query. Protocol errors are structured; stdout contains exactly one JSON reply per
input line, with diagnostics only on stderr.

Envelope: request `{id: str, op: str, ...}`; success
`{id: str, ok: true, result: object, elapsed_ns: int}`; failure
`{id: str, ok: false, error: {code: str, message: str}}`.

| Operation | Input | Result |
|---|---|---|
| `load` | `documents: list[{id,text}]`, `triples: list[{id,subject,predicate,object,valid_from,valid_to}]`, `scratch_dir: str` | document/edge counts, build and hydration timing |
| `search` | `text: str`, `limit: int` | `hits: list[{id,score}]`; tie order ID ascending |
| `format` | `triples: list[[str,str,str]]` | `text: str` from actual public formatter |
| `graph` | `method: "query_active"|"expand_neighbors"`, `entity: str`, `hops: 1|2` | directed triples, API elapsed time, returned edge count |
| `current_page` | `limit: 200` | actual dump/page API rows, API elapsed time |
| `close` | none | acknowledged close |

If implementing actual `current_page` requires a large service harness, document
the exact replicated selection and name the control `current_policy_replica`;
do not report it as a production API timing. Native graph operations use only a
fresh temporary synthetic store and stay resident. The existing traversal lacks
a scanned-edge cap; truncating its return is not bounded-work proof. Historical
and scope safeguards remain in the adapter and their cost is separately included.
Preserve directed triples; `neighbors` alone loses edge direction.
For native control construction, `assert_sync` is not evidence that adjacency was
updated. Populate the temporary store, drop and reopen it, then assert expected
neighbor counts before timing. Report this hydration cost. Empty traversal on a
known fixture edge fails the gate rather than becoming a fast timing result.

## Python interfaces

Only signatures are specified; implementations may use typed dataclasses or
TypedDicts corresponding to the records above. No abstract hierarchy is needed.

```python
def validate_fixture(sources: tuple[Source, ...], queries: tuple[Query, ...]) -> None: ...
def eligible_facts(sources: tuple[Source, ...], query: Query) -> tuple[Evidence, ...]: ...
def build_projection(sources: tuple[Source, ...], policy: Policy) -> Projection: ...
def update_projection(current: Projection, events: tuple[Event, ...], limit: int) -> Maintenance: ...
def retrieve(query: Query, treatment: Treatment, index: Projection, model: LocalEncoder, helper: RustHelper, policy: Policy) -> Retrieval: ...
def pack(query: Query, evidence: tuple[Evidence, ...], budget: int, helper: RustHelper, encoding: TokenCounter) -> Packet: ...
def evaluate(packet: Packet, gold: Gold, sources: tuple[Source, ...]) -> Metrics: ...
class LocalEncoder:
    def encode(self, texts: tuple[str, ...]) -> NDArray[np.float32]: ...
class RustHelper:
    def request(self, operation: Request) -> Response: ...
```

`Evidence`: fact identity, source digest/revision, original directed triple,
complete claim, original span, retrieval rank and lane. `Retrieval`: ordered
evidence, per-stage integer nanoseconds, seeds/scanned/emitted edge counts,
truncation flags. `Packet`: final text, exact token count, included evidence IDs,
source spans, dropped IDs, complete stage timings. Packet IDs alone are not proof
of inclusion: evaluator validates the entire claim in the emitted text.
`Policy`: version, max seeds, max hops, max scanned edges, max emitted edges,
minimum cosine. `Maintenance`: next projection, changed/removed counts, pending
event sequence, source and derived hashes. Equal input/order-independent source
sets produce equal semantic output; elapsed times are excluded from equality.

Errors derive from `ExperimentError`: `FixtureError`, `ProtocolError`,
`ModelArtifactError`, `IntegrityError`. Fail closed on missing artifacts, invalid
shape, nonfinite vectors, unsupported protocol, or source digest mismatch.
Empty eligible corpus/query is a valid empty result, not an exception.

## Treatments and matched packing

Main four treatments: `bm25`, `bm25_graph`, `bm25_dense`, `bm25_graph_dense`.
All use unsplit contextual source text (`title + body`), identical temporal
projection, and the same compact extractive packer. A graph improvement therefore
cannot be credited merely to shorter facts versus whole source bodies.
Also retain `bm25_source_packet` as a presentation ablation at the same budgets.

Controls: `standing_cache`, `current_lexical_graph`, `graph_only`.
Standing cache uses the actual hot-predicate classifier and public formatter.
Current lexical graph uses the first 200 newest active rows, hot-predicate filter,
and existing subject/object lexical-overlap policy; label a faithful replica if
the private selector cannot be invoked. `graph_only` is the new scope-partitioned
adjacency projection, with exact entity/alias seeds and bounded 1/2-hop expansion.
Measure actual resident public `query_active` and `expand_neighbors` separately
on the same seeded tasks, including a high-degree hub.

Standing facts are an independently reported stratum. For task packets all lanes
receive the same eligible standing prelude, charged to the same total token
budget; the prelude is deduplicated against retrieved evidence. Thus the four-way
comparison tests added task context, not whether a standing rule survives BM25.
Also report the no-prelude retrieval ablation if standing content fills a budget.
Gold includes acceptable standing assertions separately from task requirements.
Unsupported-query empty rate and unsupported enrichment tokens apply to the
task-dependent portion; report prelude tokens separately and total tokens too.
Task F1/coverage excludes the common standing prelude. Report standing coverage
and precision as a separate stratum; common rules cannot inflate task quality.

All selected claims use a common public-formatter adapter:
`(source identity, "is_fact", complete extractive claim)`; source/provenance stays
in the evidence sidecar. This is an experimental representation. The current
formatter itself drops arbitrary non-hot predicates, so native control triples
must not be passed through this adapter and called existing behavior. Budget is
128, 256, or 512 cl100k_base tokens for the complete enrichment block including
headings. No partial facts. An oversized claim is omitted, not truncated into an
apparently complete assertion. Stable rank then fact-ID order decides packing;
gold never determines phrase selection or inclusion.

Lexical and dense lanes rank source documents; every eligible fact in a selected
source becomes an extractive candidate in source-byte order. Graph ranks facts
by seed rank then path length then fact ID. Multi-lane fusion is fixed reciprocal
rank fusion `sum(1 / (60 + rank))`, with fact-ID tie breaks. Candidate cap is 20
source documents per lexical/dense lane and 32 graph facts. Exact aliases may
seed a graph only when unambiguous within scope. No implicit cross-scope fallback.

## Local embedding contract

Model: cached fp32 Qdrant all-MiniLM-L6-v2 ONNX at
`/Users/masa/.cache/fastembed/models--Qdrant--all-MiniLM-L6-v2-onnx/snapshots/5f1b8cd78bc4fb444dd171e59b18f3a3af89a079/model.onnx`
with its local `tokenizer.json`. Record both SHA256 hashes and runtime versions.
Observed model SHA256: `bbd7b466f6d58e646fdc2bd5fd67b2f5e93c0b687011bd4548c420f7bd46f0c5`.
Observed tokenizer SHA256: `da0e79933b9ed51798a3ae27893d3c5fa4a201126cef75586296df9b4d2c62a0`.
Use ONNX Runtime CPU, fixed thread count 1, maximum 256 model tokens, attention-
mask mean pooling, float32 L2 normalization, and cosine similarity. This differs
from cl100k prompt-budget counting. Record truncated-source counts; title/body is
identical to BM25 input before model truncation. No fake vectors or silently
substituted quantized model. Index vectors persist resident; every measured query
computes a fresh query vector. Model load and corpus encoding cost are reported
separately. A warm tokenizer cache may be required; absence must not download.

## Frozen tuning and metrics

Graph grid: `(max_seeds,max_hops) = (1,1),(3,1),(1,2),(3,2)`;
all have 128 scanned-edge and 32 emitted-edge caps. Graph seeds are literal entity
or alias mentions in the query, plus supplied entity hint. Retrieval-discovered
seeds are excluded from this first experiment to avoid another tuning dimension.
Dense minimum cosine: `0.25,0.40,0.55`. Tune graph and dense individually, then
combine the selected policies unchanged. BM25 has no new tuned parameters.
Rank all policy candidates across all three budgets on tune only: first minimize
scope/stale violations, then maximize macro fact F1, then required-fact coverage,
then minimize mean prompt tokens; exact ties use listed grid order. Freeze
selection before opening heldout labels. Latency does not select policy.
Write the selection artifact successfully before reading heldout gold.

Categories: standing preferences, exact entity/alias, one-hop relations, two-hop
relations, paraphrase with low vocabulary overlap, recent changes, historical
facts, ambiguous aliases, noisy/hub graphs, unsupported requests. Holdout should
contain new wording, not only substituted entity names. Include inverse-direction
questions, same name in two scopes, cycles, duplicate paths, expired/future facts,
and useful old facts crowded out by 200 newer irrelevant structural rows.

Per query and budget report required-fact coverage, all-required success, precision
(`acceptable emitted / all emitted`), fact F1, stale/future/conflict fact count,
scope errors, unsupported-query empty rate, unsupported enrichment tokens, total
tokens, useful facts per 100 tokens, and unique evidence count. Empty gold queries
have no coverage denominator; report them separately. Count assertions from the
actual packet, including unlabelled noise, rather than only registered gold hits.
Compare differences query-by-query and by category; no global optimum claim.

Time startup, model load, corpus encoding, projection build/hydration, resident
BM25, query embedding, dense similarity, graph lookup, eligibility, fusion,
formatting/tokenization, and end-to-end packet latency. Report p50/p95 over five
measured repetitions after one warm-up; repetitions assess timing, not independent
quality samples. Freeze query order rotated by treatment to limit order effects.
Include IPC in end-to-end latency; report graph API timing separately. Quality
is deterministic; allow documented floating-point tolerance for vector equality.

Repeat exact graph lookup, bounded graph lookup, and BM25 timing at 1x/10x/100x
deterministic noise sizes for a fixed small probe set. This is a scaling probe,
not new relevance evidence. Record RSS and index bytes, examined edges, graph
coverage of source facts, and truncation rates. A fast empty graph response does
not establish useful prompt enrichment.

## Deterministic maintenance and focused verification

Derived graph and vector records retain source revision/digest, policy version,
model hash, and factual validity. Event batches update only changed sources;
deletion removes all derived evidence. Unchanged dream cycles have no rewrites
and preserve verified/observed timestamps. Source-only retrieval remains usable
while optional derived records are absent; a batch-size limit and next sequence
support gradual upgrades. This prototype proves its own snapshot compatibility,
not a production old-release migration contract.

Focused checks: fixture/gold isolation and exact spans; scope/time/history and
revision deletion; unknown/ambiguous alias and cycle/hub budgets; full-claim token
packing and duplicate suppression; resident JSONL reuse and actual native graph
direction; local ONNX finite unit-norm vectors and unavailable-artifact failure;
incremental versus clean rebuild equality; same-state/reversed-insertion semantic
determinism. Record source hashes, commands, raw gate verdicts, and model artifacts.
