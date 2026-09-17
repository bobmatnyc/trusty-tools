# Deterministic memory experiment interface

Status: design contract, 2026-09-17. Governing experiment: [research brief](README.md).
This document specifies an isolated experiment, not a production storage schema.

## Boundary and reuse

Implement one Rust example, `crates/trusty-memory/examples/memory_deterministic_eval.rs`,
with small sibling modules if needed to meet the file cap. A Python evaluator supplies
frozen synthetic data and measures the response. No daemon, network, model, vector,
LLM, production database, source deletion by age, or free-text contradiction inference
belongs in this path. No service container or new polymorphic abstraction is needed.

Use actual `trusty_common::bm25::BM25Index::{upsert_document_reporting,
remove_document,score_query_all_with_filter}`. Its tokenizer and score computation
remain authoritative (`crates/trusty-common/src/bm25.rs:72,333,497`). Check the
reported insertion result: a capacity rejection is an error, never successful coverage.
Use `trusty_memory::bm25_index::PalaceBm25Index` in legacy compatibility tests;
`crates/trusty-memory/src/bm25_index.rs:65` defines its `{doc_id,text}` disk rows.
Existing `Drawer` fields and types are documented in
`crates/trusty-common/src/memory_core/palace.rs:122,219`; extra time/alias fields below
are experiment inputs, not claims that production already persists them.

## CLI and transport

```text
cargo run -p trusty-memory --example memory_deterministic_eval -- --format json
cargo run -p trusty-memory --example memory_deterministic_eval -- --format jsonl
```

Default format is `json`: one request on stdin and one response on stdout, followed
by newline. `jsonl` accepts independent one-line requests and emits one response per
line. No hidden process state carries between requests; reuse the returned `state`
explicitly. A fresh process receiving that state is the restart test. UTF-8 JSON
only; all diagnostics go to stderr. Timings are observations, excluded from hashes.

Exit 0 means every request succeeded; exit 2 means validation/contract error; exit 1
means I/O/internal failure. JSONL continues after a recoverable request error and
exits 2 after EOF if any failed. Invalid JSON returns a null `request_id`.

## Schemas

Object keys below are exact. Unknown request/model keys are rejected, except query
label keys consumed by Python (Python removes these before sending the request).
Dates are UTC RFC3339 strings with `Z`; normalize equivalent instants before hashing.
All optional fields explicitly accept null; omitted fields use the defaults shown.
IDs are nonempty opaque strings, compared bytewise. No identifier is a filesystem path.

### Drawer and mutation

```text
Drawer = {
  id: string, scope: string, room: string, body: string,
  tags: string[] = [], fact_key: string|null = null,
  created_at: Timestamp, effective_at: Timestamp|null = null,
  verified_at: Timestamp|null = null, expires_at: Timestamp|null = null,
  valid_to: Timestamp|null = null,
  kind: "UserFact"|"SessionEvent"|"AgentNote"|"Commit"|"Unknown"|"Task" = "Unknown",
  importance: number = 0.5, aliases: string[] = [], links: Link[] = []
}
Link = {
  target_id: string, predicate: string,
  valid_from: Timestamp|null = null, valid_to: Timestamp|null = null
}
Mutation = {op:"upsert", revision:positive_integer, drawer:Drawer}
         | {op:"remove", revision:positive_integer, scope:string, id:string}
```

Identity is `(scope,id)`. `room` is an explicit context label within that scope.
Importance is finite in `[0,1]`. Sets (`tags`, `aliases`, `links`) are deduplicated
and stably sorted for identity. Body bytes remain exact. Intervals must have
`valid_to > effective_at` when both exist; link intervals obey the same rule.
Unknown dates remain unknown. Links target only the same scope, never a global ID.
Dangling links are allowed but yield no candidate. Aliases are explicit names;
no alias generation or body entity extraction is allowed.

Revisions are monotonic per identity, including tombstones. A lower revision is an
acknowledged ignored mutation. An equal revision with identical canonical content
is an idempotent retry; different content is `revision_conflict`. A remove wins an
equal-revision upsert. Only an explicitly higher revision can recreate a removed
identity. Validate the entire request before applying any mutations.

### Request and query

```text
Request = {
  schema_version:1, request_id:string,
  treatment:"raw"|"repaired_raw"|"context"|"chunks"|"temporal"|"kg",
  as_of:Timestamp, state:State|null = null, policy:Policy = defaults,
  mutations:Mutation[] = [],
  maintenance:{max_documents:nonnegative_integer,
               max_bytes:nonnegative_integer,max_edges:nonnegative_integer},
  queries:Query[] = []
}
Query = {
  id:string, text:string, scope:string,
  mode:"current"|"asof"|"general", as_of:Timestamp,
  knowledge_cutoff:Timestamp|null = null, top_k:integer = 5
}
Policy = {
  context_tokens:positive_integer = 64, chunk_tokens:positive_integer = 256,
  freshness_weight:number = 0.05, kg_weight:number = 0.15
}
```

`top_k` is in `[1,100]`; empty text yields no lexical hits. Request `as_of` pins
maintenance metadata only. Query `as_of` pins eligibility and scoring; neither
uses the wall clock. Null `knowledge_cutoff` means no additional recorded-time
cutoff. Each query scope is one exact allowed scope; room names are not authority.
Policy bounds are context tokens `[1,256]`, chunk tokens `[32,1024]`, freshness
weight `[0,0.20]` and KG weight `[0,0.30]`, all finite. Python predeclares the small
tuning grid, selects only on tuning families, and freezes policy before holdout.

### Portable state

```text
State = {
  schema_version:1, policy_version:"memory-deterministic-v2",
  treatment:Treatment, policy:Policy, generation:nonnegative_integer,
  sources:[{drawer:Drawer, revision:positive_integer}],
  tombstones:[{scope:string,id:string,revision:positive_integer}],
  snapshot:[{doc_id:string,text:string}],
  derived:[{doc_id:string,scope:string,id:string,revision:positive_integer,
            fingerprint:string,body_digest:string,
            byte_start:nonnegative_integer,byte_end:nonnegative_integer,
            line_start:positive_integer,line_end:positive_integer}],
  cursor:string|null
}
```

Arrays are sorted by identity/doc_id. `doc_id` is the JSON serialization of
`[scope,id,child_index]`, where unsplit/legacy uses child index 0. Byte ranges are
half-open UTF-8 boundaries; line ranges are inclusive and one-based. Empty bodies
use bytes `[0,0)` and line 1. `body_digest` is lowercase SHA-256 of original bytes.
Fingerprint is SHA-256 of canonical drawer data, source revision, treatment,
tokenizer version (`trusty-common-bm25-v1`), policy version and values, and applicable explicit
link-target aliases/revisions. Canonical objects use lexicographically sorted keys,
compact UTF-8 JSON and normalized dates. Hashing must not depend on map order.
Index build time and access time never enter a verification timestamp.

Source state is authoritative; snapshot/derived entries are optional coverage.
Importing a populated snapshot with an empty `derived` array is valid legacy state;
it serves immediately without requiring full repair. Legacy import fixture IDs
must use the identity encoding above so scope/source hydration is unambiguous.
Snapshots exported to disk are the array alone: old readers see precisely
`[{"doc_id":"...","text":"..."}]`, without an envelope or new required fields.
A legacy row has no revision proof and therefore does not count as fresh coverage.
Unknown state/policy versions and changing treatment/policy on an existing state are errors.
The evaluator starts each treatment from the same initial corpus and event stream.

### Response

```text
Response = {
  schema_version:1, request_id:string, ok:true, state:State,
  maintenance:{processed:integer,rewritten:integer,removed:integer,
               bytes:integer,edges:integer,pending:integer,
               fresh:integer,total:integer,cursor:string|null},
  results:[{id:string,hits:[Hit]}]
}
Hit = {
  id:string,scope:string,rank:positive_integer,score:number,bm25_score:number,
  excerpt:string,body_digest:string,revision:positive_integer,
  byte_start:integer,byte_end:integer,line_start:integer,line_end:integer,
  created_at:Timestamp,effective_at:Timestamp|null,verified_at:Timestamp|null,
  expires_at:Timestamp|null,valid_to:Timestamp|null,
  status:"current"|"historical"|"unknown",
  freshness_basis:"verified"|"effective"|"created"|"none",
  origins:("bm25"|"kg")[],index_fresh:boolean
}
ErrorResponse = {
  schema_version:1,request_id:string|null,ok:false,
  error:{code:string,message:string,path:string|null}
}
```

No error response contains an apparently committed replacement state. Hit excerpts
always come from the indexed revision; stale rows with unavailable original ranges
return the current whole body with `index_fresh:false` and current source digest,
never a fabricated old locator. This fallback is measurable stale selection, not
proof that the index matched the excerpt. Deleted/missing sources never hydrate.
`score` must be finite; ranks are unique and contiguous. A query returns at most one
hit per source identity. All final ties use identity then child byte-start order.

## Treatment contracts

| Treatment | Derived text / policy |
|---|---|
| raw | Body only; maintenance fills absent IDs but does not repair existing text. This deliberately models ID-only backfill, not every production write path. |
| repaired_raw | Body only; revision/dependency-aware repair and removal. |
| context | Repaired raw plus explicit room, tags, fact key and aliases, capped at policy.context_tokens added tokenizer tokens per document. |
| chunks | Context plus deterministic structural splits, at most policy.chunk_tokens lexical token occurrences and 4096 UTF-8 source bytes per child; group by source using best child score. |
| temporal | Chunks plus eligibility and typed freshness described below. |
| kg | Temporal plus bounded one-hop explicit links and exact normalized alias seeds in query scope. |

Context caps apply after stable term deduplication; body text is never deduplicated.
Chunks prefer existing heading/paragraph/list boundaries; oversize segments split
at whitespace, then UTF-8 scalar boundaries for oversized single runs. Body cost
is the sum of `trusty_common::bm25::tokenize(run).len()` over whitespace-delimited
runs: repeated runs count repeatedly, and compound-identifier expansions count
within each run. The tokenizer's deduplicated whole-document vocabulary is not a
length measure. BM25 indexing/scoring is unchanged. A fixed 4096-byte source-body
cap also applies to each child; it is a safety constant, not a tuning parameter.
Added context remains subject to its separate context and publication budgets.
Policy identity v2 distinguishes these boundaries from the superseded v1 experiment.
Exact source ranges
remain available. Query-time scope filtering and source-existence filtering apply
before truncation for every treatment. Re-rank all eligible lexical matches for
this small experiment before top-k so freshness cannot only act on an arbitrary
truncated set. BM25 returns its real scores, not Python reimplementations.

Every treatment preserves strict expiry filtering and explicit knowledge cutoffs.
Temporal/kg additionally enforce validity intervals and single-valued slots; earlier
ablations expose those errors to the evaluator. Current/asof use `effective_at` when provided,
otherwise `created_at`, as interval start, with exclusive `valid_to`. Expiry follows
production's strict `< as_of` rule (`Drawer::is_expired`), so equality is retained.
Every mode applies an explicit knowledge cutoff to `created_at` when supplied.
Current chooses latest eligible occupant per non-null `(scope,fact_key)`; ties use
created date then ID. This is a declared single-valued slot, never arbitrary prose.
As-of uses the same rule at query time. General allows retained historical records,
marks their status, and does not present them as current. No treatment deletes a
source merely because it is old, expired, superseded, or a completed task.

Temporal score is `(0.90-freshness_weight) * lexical_rank_score + 0.10 * importance + freshness_weight * freshness`,
where lexical rank score is `1/rank` after grouping, and freshness is
`2^(-max(age_days,0)/half_life_days)`. Date basis is verified, effective, then created;
future basis dates contribute freshness 0. Frozen half-lives: UserFact/Task 3650,
Commit 365, AgentNote 90, SessionEvent 7, Unknown 3650. Dates do not assert truth.

KG selects at most 32 valid edges per query and at most 16 extra candidate identities,
in stable `(seed identity,predicate,target identity)` order. Link intervals use query
time. Exact alias seeds require the whole normalized query to equal an explicit
alias; ambiguous matches remain separate sources. All target eligibility and scope
checks run before ranking. KG-only candidates have lexical rank score 0 and add
policy.kg_weight graph evidence; lexical candidates reached by a valid edge receive the same
bounded addition. KG-only hits have BM25 score 0. No multihop traversal or inferred
supersession is allowed. Missing targets and capped candidates are not source loss.

## Maintenance and restart contracts

Maintenance is independently callable with `queries:[]`; queries are independently
callable with all budgets zero. Budgets cap source documents selected for publication,
UTF-8 body plus added-context bytes accepted for publication, and dependency edges
inspected. An item must fit all budgets atomically; oversized items produce
`budget_too_small` naming identity and required minimum, rather than starving forever.
Zero document or byte budgets intentionally perform no maintenance and are not errors.
Zero edge budget still allows drawers with no dependency edges. A record that fits
a fresh batch but not its remaining budget defers to the next call without error.
Portable-state parsing, BM25 reconstruction and exact coverage/fingerprint accounting
can inspect the full supplied state. These publication budgets do not bound total
request CPU or bytes read; report those costs as experiment overhead. A production
adapter would need persistent dependency/fingerprint tracking before claiming bounded
total maintenance work. No such adapter is part of this experiment.

The persisted cursor is the last inspected identity, encoded as JSON `[scope,id]`.
Each positive batch advances in stable identity order, wrapping for reconciliation;
new early IDs cannot starve later IDs. Removals are included in budget accounting;
tombstone/source filtering prevents stale visibility before physical cleanup.
Pending/fresh counts compare revisions and fingerprints, not ID coverage. Raw may
retain pending stale rows intentionally; the evaluator never drains raw until fresh.
An unchanged complete pass has rewritten=removed=0 and does not advance generation.
Cursor advancement alone is durable state progress, not an index generation change.

Generation increases once per request that changes snapshot or derived rows, after
all selected related rows are complete. Each drawer's children publish together;
replacing or removing a drawer removes obsolete children. Identical requests against
identical input state return identical semantic JSON. Interrupted output is not a
committed state: the evaluator keeps its previous complete response and retries.
Out-of-order updates cannot lower a source revision or resurrect a tombstone.

## Public signatures and errors

The example exposes these internal module contracts for focused tests; no public
crate API change is required. Rust declarations below are signatures, not bodies.

```rust
fn evaluate(request: Request) -> Result<Response, ExperimentError>;
fn validate(request: &Request) -> Result<(), ExperimentError>;
fn fingerprint(drawer: &DrawerInput, revision: u64, treatment: Treatment,
               dependencies: &[DrawerInput]) -> Result<String, ExperimentError>;
```

`ExperimentError` variants map to codes `invalid_json`, `invalid_request`,
`unsupported_version`, `revision_conflict`, `invalid_state`, `budget_too_small`,
`index_capacity`, `io`, `internal`. Validation errors include a field path.
State corruption is an error; never silently reset to empty. There is no fallback
to vector search, a daemon, or a model. Python raises its local `EvaluationError`
for nonzero exit, invalid response, mismatched request ID, or violated invariants.

## Acceptance evidence

Implementation tests must cover same-ID body and metadata repair, unchanged rerun,
delete/tombstone ordering, budget resume, serialization restart, mixed legacy/derived
state, snapshot loading by `PalaceBm25Index`, scope filtering before top-k, interval
boundaries, unknown dates, alias ambiguity, stale locators, and insertion rejection.
Python must keep labels out of engine requests, hash frozen corpus/queries/policy,
and report lexical relevance separately from temporal validity. Tests are future
requirements here; this design document does not claim executed retrieval results.
Absence of embedding/inference is established by the experiment's dependency and
call paths plus runtime tests; a hardcoded zero counter is not instrumentation.
