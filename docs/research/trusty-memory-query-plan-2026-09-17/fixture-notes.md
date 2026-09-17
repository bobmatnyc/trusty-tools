# Query-plan fixture and independent author audit

Authored after reading the frozen interface at `/tmp/queryplan-interface.md`, without reading the new implementation or running retrieval. Only schema/predicate/grammar information was exchanged with the interface designer. Query text and gold were not sent to the engineer. Earlier exposed fixtures remain unchanged.

## Counts and novelty

The fixture contains92 initial sources,100 exact-span assertions,8 revision/tombstone events, and64 queries. Tuning families are Bracken Engine and Riverstone Array. Heldout families are Cinder Beacon and Fathom Spindle. Exact entity/alias vocabularies do not overlap across splits. All hints are null. The existing `graph-prompt-v1` schema remains unchanged.

| Split | Unique queries | Positive | Negative | Required groups |
|---|---:|---:|---:|---:|
| Tuning |32|18|14|22|
| Heldout |32|20|12|40|

Each family has16 queries. Two entities per split share each task structure, so these are not64 independently sampled structures. Budgets128/256 and timing repetitions must be reported separately from unique query counts.

Exactly16/32 heldout queries use eight compositions absent from tuning, each instantiated in two families:

1. Different relations bound independently to two entities.
2. Inverse maintenance followed by a location property.
3. Dependency-to-location with exclusion of one branch target.
4. Ambiguous alias enumeration followed by each referent's location.
5. Dependency-to-release with one supported endpoint and one missing endpoint.
6. Explicit qualified prohibition with exclusion of approval-rule output.
7. Four required semantic outputs across clauses, with duplicate ownership evidence competing for the cap.
8. Historical ownership plus output exclusion of maintenance.

The remaining16 heldout queries reuse the following basic scenario primitives: overlapping names/presentation constraint; supported unfamiliar wording; unresolved longer entity; knowledge cutoff; source expiry; deletion; qualifier mismatch/unsupported qualifier. Qualifier negatives occupy two positions per family. Wording and names differ, but these primitives are not claimed as structurally independent. Corpus relation vocabularies are deliberately shared. This is authored grammar evaluation, not a natural-language workload sample.

All positive required evidence fits four semantic assertions at most. The alias-to-location task requires four assertions; its path connections are required. The dedup-cap task requires owner, location, release rule and rationale, also four. The standing prelude is outside the semantic task cap but inside the token budget. No positive target is impossible solely because of the declared four-fact cap.

## Labels and evidence

Every claim is a complete exact UTF-8 span in source prose and matches its declared triple. No answer/predicate annotation appears in query fields beyond the actual prompt text. Explicit release approval and release prohibition coexist on the same subject. Only the stored prohibition with literal `after 19:00 UTC` supports the matched qualifier request. The `after 20:00`, `after 21:00`, and `before 08:00` requests have no supporting fact; they must not be satisfied by that broader subject match. The unrelated prohibition identity is forbidden for those queries even though it is temporally valid. Thus `forbidden` includes query-incompatible evidence, not only stale/cross-scope facts.

Duplicate owner assertions differ only in source provenance. Their full semantic tuple, validity and expiry agree; either satisfies one required group. Emitting both earns one useful fact and two assertion/token costs. Current ownership and historical ownership differ in value and validity and must not consolidate. Owner and maintainer have different predicates and different values; labels never treat them as alternatives.

The partial-branch task requires the existing dependency edge and that dependency's release property. The connector for the other, unsupported branch is acceptable supporting context, but no missing release property is invented. A partial execution diagnostic is appropriate even when all answerable gold groups are covered. Independent gold recall and execution completeness are distinct metrics.

The long-name query names a known exact suffix entity, while unknown Observatory names intentionally contain known shorter prefixes. Explicit quotes define full reference boundaries. A follow-on `its location` after an unresolved Observatory must not inherit a known shorter root.

Unsupported positive wording remains positive: tuning asks about an underlying consideration rather than a listed rationale phrase; heldout asks for custodial accountability rather than the literal owner relation phrase. These cases measure the cost of conservative grammar abstention. They are not reclassified as negative when parsing fails.

## Expected temporal states, independently specified

- At the initial September1 clock, the July1 owner and its exact duplicate are valid. The former owner closed July1 and is forbidden in current-owner queries.
- At June1, only the former owner is eligible; July1 observations and effective dates exclude the newer owner. Historical gold requires the former owner and forbids both current duplicates.
- A Locker location is learned September10, so it is unavailable under every September1 knowledge cutoff even though its effective interval starts May1. Its longer entity name must not resolve to the base entity's location.
- A Gate access code expires August1 and cannot satisfy September1 access requests. The subject may disappear from the eligible catalog; unresolved is distinct from a wrong base-entity answer.
- Every family has a revision2 release replacement observed/effective August20. Initial scenarios use revision1; updated scenarios apply complete source replacement and use revision2. The replacement statement and timestamps are retained together.
- Every family also has a revision2 Archive tombstone observed August20. Updated scenarios remove its source and all evidence. Deleted queries forbid the original Shelf2 assertion.
- An external scope contains the same base entity name with another owner. It is forbidden for every query in the family's own scope.

An independent raw-JSON audit reconstructed scenario state by applying events, compared parsed timestamps rather than string order, evaluated intervals/TTL/cutoff, and selected maximal single-value slots while retaining ties. It checked accepted and required gold against those states, disjoint forbidden labels, every span and identity, and split-file equality. This did not call the implementation's eligibility function. The previous strict schema/split parser was an additional check.

```text
tune 32 positive 18 negative 14 groups 22
heldout 32 positive 20 negative 12 groups 40
novel_heldout 16
PASS independent timestamp/event/slot eligibility and gold/span audit; legacy strict schema/split validation
```

No retrieval or model was run. Parent performs independent review and freezes hashes before measurement. Only the five fixture JSON files and this note were authored.
