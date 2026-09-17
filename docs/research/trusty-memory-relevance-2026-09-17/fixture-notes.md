# Independent relevance fixture

Authored before retrieval or tuning, 2026-09-17. The fixture author inspected the prior experiment's mechanisms but did not run its ranking against these inputs. Only predicate names and the existing schema were shared with the policy designer; heldout query wording and gold were not shared. Sources are entirely fictional, explicit assertions. This evaluates deterministic retrieval of curated facts, not automatic extraction.

## Corpus and split

The corpus has 86 initial sources with 180 exact-span assertions, eight revision/deletion events, and 96 queries. Tuning uses Zephyr Mill and Lumen Forge; heldout uses Atlas Loom and Meridian Press. These names do not reuse the previous experiment's families. Every query has `entity_hint: null` and canonical second-resolution UTC clocks. Schema remains `graph-prompt-v1`.

| Split | Unique queries | Positive tasks | Task-empty negatives | Required fact groups |
|---|---:|---:|---:|---:|
| Tuning |48|28|20|40|
| Heldout |48|28|20|44|

No query is standing-only. Each scope has one legitimate standing preference, acceptable in every query and excluded from task relevance metrics. The standing prelude still consumes prompt tokens. Repeated budgets and timing repetitions must not inflate the count of independent queries.

Every fact claim is a complete exact UTF-8 span in its source body. Triple metadata is explicit and agrees with the claim. Entity relationships have `object_entity`; literal properties do not. All sources deliberately share the family's operations-notebook title except the external-scope distractor. The long mixed note contains nineteen assertions, with the relevant access instruction surrounded by eighteen calibration assertions. This tests document-to-claim expansion without the prior fixture's hundreds of inventory tags.

## Structures and relevance cases

Each family includes duplicate ownership assertions in separate sources, current and historical ownership, a location, a release prerequisite, an explicit rationale, an incident route through a team to a person, aliases and ambiguous aliases, overlapping entity names, tied conflicting ownership values, a dependency cycle, mixed-source access instructions, expired access, a fact learned after the cutoff, and a deleted source. A same-name entity in another scope is a forbidden distractor.

Tuning dependency questions follow one dependency and its property. Heldout questions compare two immediate dependencies and require both connecting edges and both release properties. Heldout also asks for an explicit multi-item handoff, complete incident-route attribution, preserved ownership disagreement, and selective extraction from the mixed note. Heldout wording differs throughout, including negatives that distinguish a requested telephone number from a known calling window, a founding date from later milestones, and an explicit prohibition from a release prerequisite. Some scenario primitives are necessarily shared; this remains a small authored distribution, not an independent natural-query sample.

Fourteen positive tasks per family cover: owner with duplicate alternatives, location plus release requirement, rationale phrased differently from the predicate label, incident routing, inverse maintenance, both meanings of an ambiguous alias, exact longer-prefix entity, both conflicting values, historical owner, replacement release rule, mixed-note access requirement, a partially supported multi-request, explicit alias resolution, and dependency prerequisites.

Ten negatives per family cover: missing rationale for an entity that has other facts, unknown longer-prefix entity, knowledge-cutoff exclusion, expired access, deleted evidence, unknown forecast intent, undocumented emotional reaction, missing telephone predicate, unknown founding date, and absent explicit negated rule. A rationale is supported elsewhere, preventing a blanket rejection of “why” questions from being universally correct.

The partial-answer query asks for location and founding date, but only location is documented. Its gold requires the available location and does not invent an answer to the missing part. Gold measures selected factual evidence, not whether generated prose explicitly announces uncertainty; no answer model runs.

## Duplicate and temporal contracts

The two current owner assertions have identical scope, subject, predicate, object, entity target, validity, expiry, and standing state. They are alternatives in one required group. Either source satisfies the group; both may be acceptable provenance, but emitting both earns one useful semantic fact and two assertion/token costs. Distinct conflicting owner values remain two required groups and must never consolidate.

Each family contributes a revision-2 release rule and a revision-2 source tombstone. Updated tasks forbid revision1 release evidence and require revision2; deleted tasks forbid removed location evidence. Replacement rules become effective and observed August20. Initial and updated queries use September1; history uses June1. Historical ownership closes July1. Expired access closes at source TTL August1, while a future-known location is observed September10. No query-time clock is inferred from its wording.

The heldout dependency comparison requires four fact groups. At a small prompt budget, failure to fit a complete path is a meaningful packing outcome, not permission to relabel required facts after results. Alias ambiguity questions require both alias assertions; they do not require selecting either referent's unrelated facts.

## Author verification

Loaded sources/events/queries through the prior strict `records.load_inputs` parser and split validator without invoking indexing or retrieval. Independently checked every gold identity against all revisions, each accepted fact against scenario-specific eligibility, required groups against acceptable evidence, forbidden disjointness, null hints, and exact split-file equivalence. Parser verifies exact claim spans, duplicate identities, intervals, and increasing event revisions.

```text
tune queries 48 positive 28 negative 20 required_groups 40
heldout queries 48 positive 28 negative 20 required_groups 44
PASS: prior strict schema/split validator, exact spans, all gold eligible, no forbidden overlap, event revisions and partitions
```

Only these fixture files and this note were authored. Generation/audit scripts are external scratch artifacts, not experiment implementations. Parent freezes the manifest after policy and independent review. No retrieval output has informed these labels.


## Pre-run independent audit correction

Before any ranking, independent review found that four incident-route gold rows accepted the team incident-channel assertion although the questions ask for the responder and calling hours. The channel is neither required nor part of the person-routing evidence path. Removed that identity from acceptable evidence in canonical gold and both split files. Required groups, sources, queries, and policies are unchanged. The author audit passed again with the same counts.

Heldout structural novelty is limited: most categories and underlying templates remain shared with tuning. Only the two dependency-comparison queries add a branched task structure, representing 2 of 48 heldout queries. Different wording elsewhere does not establish an independent task distribution. No query or policy was changed in response to this observation.
