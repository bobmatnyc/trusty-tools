# Relevance fixture audit

Verdict: PASS on schema, source evidence, temporal labels and duplicate semantics.
The identified label correction is verified; inputs are ready for manifest freeze.
Audit date: 2026-09-17. Read-only independent audit; no ranking or fixture edits.
Heldout wording and labels were shared only with the parent, not the implementer.

## Observed checks

Files audited: `experiments/trusty-memory-relevance/{sources,queries,gold,gold-tune,gold-heldout}.json`.

| Check | Observed result |
|---|---|
| Sources/events | 86 initial sources, eight revision events |
| Evidence | 180 initial assertions, 184 assertion-revision identities including replacements |
| Tune | 48 queries: 28 positive, 20 negative, 40 required groups |
| Heldout | 48 queries: 28 positive, 20 negative, 44 required groups |
| Exact duplicates | Four classes of two equivalent assertions; four redundant assertions |
| Query hints | All 96 `entity_hint` values are null |
| Split separation | Previous parser's entity/alias-disjoint check passes |
| Gold partitions | Split files equal their corresponding canonical-gold subsets |
| Temporal/span audit | Independent datetime/revision/required/span audit errors: `[]` |

The unchanged previous parser validated source shapes, exact byte spans and split
separation. An independent datetime-based check also verified required facts after
scope, source revision, deletion, observation cutoff, expiry, validity interval,
and single-value-slot selection. All required alternatives are eligible. Every
acceptable evidence identity is eligible; acceptable and forbidden sets do not
overlap. `expected_empty` agrees with empty required groups for all queries.

Manual evidence review covered the two distinct wording families and all 24 task
forms in each. Required multihop groups contain connecting maintenance and
escalation assertions plus the endpoint's contact window. Dependency questions
require both the edge and release prerequisite for each requested branch. Those
groups are necessary support, not arbitrary adjacent facts. Owner duplicate
alternatives represent the same assertion, while two conflicting lab owners stay
separate. Historical owner facts have a closed interval and cannot collapse into
the current owner. Replacement and deletion labels refer to the correct scenario.

## Finding and accepted correction

The four `*-route` gold rows initially included an optional `uses_channel`
assertion in `acceptable`, although their prompts request the responder path and
calling hours. The channel is neither a required answer nor part of that path.
The parent accepted removal from canonical and split gold before freeze. Required
groups, prompts, source facts and policy remain unchanged. Verification of the
corrected files passed: all four optional channel labels are removed, each route
still has three required support groups and four acceptable assertions including
the common standing preference, and canonical/split gold agree. Sources and
queries retain their original hashes.

## Limits of this fixture

Heldout uses different entities and wording, but most task structures are shared
with tuning. The dependency-prerequisite cases add a second branch: two of 48
heldout queries have that structural change. Each split repeats its 24 task forms
over two entity families. Do not describe 48 independent heldout task structures
or broad natural-language validation. The experiment can measure mechanism
differences on controlled tasks and unfamiliar phrasing.

The fixture includes inverse maintenance, two-hop support, cycles, alias
ambiguity, overlapping names, two requested relations, mixed long sources,
duplicates, conflicts and unsupported questions. It does not cover every frozen
policy branch; for example, three-demand limits and inverse dependency recognition
need focused tests rather than claims of dataset coverage. No changes to query
wording or policy are recommended after this audit's heldout exposure.

## Initial audited hashes

- sources.json: `c1b703f79e95961cf72591264e3e2b8f1c5e228a7b19f9b3320376a03b125b1a`
- queries.json: `f65faf434c469e49aca5836dbeb35e83f5a1a7716a07582ebe0c6bf309e75afd`
- gold.json: `4cac805b9fb11bccf6e8f390748af7e3b2eeddf2a290db1d7ea5a1c811fdd858`
- gold-tune.json: `1d24679fc77091fee3c15127e68a2c070eef3736c95fa55423f19b05cf7a987d`
- gold-heldout.json: `7a0d5c01579e080c423003888f5f9e8a04f731941ba46a16b8d2b27603a44547`

## Verified corrected gold hashes

- gold.json: `5aca02de6cbf75f8cef07aca8c86e218ee4edfc36c6498370a73511adbfbb8d1`
- gold-tune.json: `d5f57848f59e980a262803219ab9e4698c5c02724926a8763cf45a2af0264238`
- gold-heldout.json: `a17066459f5e406eeff3e6398cb86145529a2263b25b0644bdf9e3e0da84af94`

Parent continues final freeze and implementation. No code, Git, ticket or live
memory operations occurred in this audit.
