# Frozen synthetic evaluation protocol

This protocol tests the mechanisms in the [proposal](README.md) using the
[interface](interface.md). It does not validate production retrieval quality.
The corpus is wholly synthetic: invented institutions, people, records, dates,
and labels. It contains no private memories, scraped sources, model-generated
answers, embeddings, or external inference calls.

## Frozen inputs

The experiment directory contains `fixture.json`, `queries.json`, and
`manifest.sha256`. The manifest records SHA-256 digests of both JSON files and
this protocol before retrieval runs or tuning. Verify it from the repository
root with `shasum -a 256 -c experiments/trusty-memory-deterministic/manifest.sha256`.
An input change requires a new freeze and rerunning every treatment; never edit
labels to fit observed rankings. Record implementation revision, parameter
digest, runtime version, and request/response artifacts separately.

There are 102 initial memories, 18 dated mutation events, and 66 queries. Cedar,
Harbor, and Quartz are tuning families; Velvet, Meadow, and Tundra are held out.
Each split has 33 queries. All queries for an entity and its fact families stay
in one split. Families share scenario templates and lexical structures, so this
is entity-disjoint synthetic evaluation, not independent real-query validation.
Held-out labels must not select weights, chunk sizes, expansion bounds, or
half-lives. Freeze those choices from tuning results before a single held-out run.

## Scenarios and labels

| Category | Queries | Evidence and expected behavior |
|---|---:|---|
| Current owner | 6 | Explicit fact key and July interval select new owner |
| Historical owner | 6 | June interval and knowledge cutoff select previous owner |
| Durable preference | 6 | Old explicit preference survives recent irrelevant suggestions |
| Protected task | 6 | Open task survives age; no implicit completion |
| Long manual | 6 | Exact repair detail in one paragraph; other paragraphs are unrelated |
| Scoped alias | 6 | Structured alias locates source; other-scope homonym is forbidden |
| One-hop KG | 6 | Exact registry alias links to a same-scope storage record |
| Knowledge cutoff | 6 | June-effective assertion recorded in August is unavailable in June |
| Expiry | 6 | August admission code cannot answer September access |
| Unsupported | 6 | Corpus has no founder motivation; no relevant source |
| Same-ID content update | 2 | New gate excerpt required after update; old excerpt invalid |
| Metadata-only update | 2 | Body stays identical; new room/tags/alias become searchable |
| Deletion | 2 | Deleted travel note cannot return after maintenance |

`expected_ids` and `relevant_ids` are identical binary source-level labels.
`invalid_ids` identify temporally unavailable, superseded, expired, or deleted
sources. `forbidden_ids` identify scope violations. These are independent error
sets, not relevance grades. Any out-of-scope result is forbidden even if its ID
was not enumerated. Unlabelled same-scope sources are irrelevant, not proven
false. Duplicate chunks of one source count once for recall and reciprocal rank.
`required_evidence` and `forbidden_evidence` apply to returned source excerpts;
same-ID updates cannot pass solely by returning an expected ID.

The modes are `current`, `asof`, and `general`. Initial current/general queries
use 2026-09-01T00:00:00Z. Historical queries use 2026-06-15T00:00:00Z. Each query
pins `knowledge_cutoff` equal to `as_of`; no runtime clock or access timestamp
can substitute for either. Created time controls whether an assertion was known.
Effective time controls its explicit validity. Owner intervals are [start,end).
Expiry samples precede the cutoff by one second, avoiding a claim about changing
the existing interface's strict expiry boundary. Missing confirmation dates stay
unknown. General queries still enforce scope and the interface's expiry policy.

Initial queries run on revision-1 drawers before events. Postevents queries run
at 2026-09-05T00:00:00Z after applying all events in ascending `(as_of,id)` order.
The three event dates independently change body, change metadata, and delete a
source. Each mutation uses revision 2. Authoritative stores receive all events
for every treatment; whether the derived index catches up is the treatment.
Keep raw initial indexing stale when measuring the existing ID-only refresh
limitation. Do not rebuild raw from postevents bodies and call that repair gain.

## Measurements and selection

Report per split, category, scenario, and treatment. Use Recall@5 as relevant
source hits divided by the number of relevant source IDs, and MRR as reciprocal
rank of the first relevant source, with misses zero. Average those metrics only
over queries with nonempty relevance labels. Empty-label queries instead report
empty-result rate, nonempty-result rate, invalid-hit rate, and forbidden-hit rate.
Returning an unrelated candidate is not an invented answer because the experiment
does not generate answers. Never assign perfect recall or MRR to an empty label.

Report invalid-hit queries divided by all queries and by the relevant temporal
category denominator. Report forbidden hits as both count and query rate. Report
same-ID excerpt checks separately from ID relevance. A zero invalid-hit rate does
not establish that every returned item is factually current: labels are bounded.
For chunks, report duplicate source share before grouping and after grouping.
Measure payload with the actual tokenizer if available; otherwise label character
or whitespace counts as proxies. Measure p50/p95 latency with a documented timer,
warmup, repetition count, and fixed request order. Never claim CPU/RSS, expansion
follow-ups, index size, or model-call counts unless directly instrumented.

Compare raw, repaired_raw, context, chunks, temporal, and kg on the same inputs.
Select parameters only on tuning data. Prefer zero forbidden/invalid hits and
passing maintenance invariants before relevance gains; break equal relevance
results using smaller payload and simpler treatment. Record failed and unsupported
queries by ID. Direct alias queries test explicit scoped aliases. One-hop queries use an exact
registry alias and a labelled `storage_record` edge to the answer source; the seed
itself contains no storage answer. These controlled cases do not establish general
relation reasoning, expired-edge correctness, entity resolution, or semantic
paraphrase understanding. A synthetic gain cannot be extrapolated to private user memories.

## Maintenance checks

Run these separately from ranking metrics: unchanged repeat makes no mutations;
same-ID body and metadata changes alter the derived digest; protected old Task
survives; deletion removes all children and postings; revision-1 stale upsert after
revision-2 removal cannot resurrect a drawer; duplicate revision is idempotent;
small document/byte/edge budgets resume to the same final generation as a full
pass; serialize/restart resumes without lost work; equal-score IDs sort stably.
Compare source stores before and after maintenance to detect unintended loss.
Replay each check with fixed inputs and compare semantic outputs excluding timing.

These checks are a required observation list, not claims they already pass.
Any unimplemented or unmeasured check must remain listed as a gap in the report.
Experiment serialization roundtrip alone does not prove old-release or production
daemon compatibility. Production adoption requires that separate integration work.
