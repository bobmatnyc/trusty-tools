Added

- Investigation findings can now carry CWE weakness classes, so a consumer counting ISO-5055-style structural flaws no longer has to infer them from prose ([#6779](https://github.com/bobmatnyc/trusty-tools/issues/6779)).
  - **Schema:** the investigation response schema declares an optional `cwe_id` array and names the weakness classes the crate reads back, so the model picks from a known vocabulary rather than inventing a spelling.
  - **Ingestion:** `report::investigate::cwe::resolve_all` admits, per entry, a well-formed `CWE-<number>` id (upper-casing it) or a class NAME from one table of 12 weakness classes, then de-duplicates. Anything else — a malformed id, an unknown class, an empty entry — is dropped; one bad entry never costs a good one, a finding is never rejected for this field, and an id is never repaired into a neighbouring one.
  - **Serialisation:** `cwe_id` is skipped when empty, so a finding with no identifiable weakness class carries no field at all in `investigation.json`. A GREEN finding names a strength, so it carries no weakness class either.
  - **Report:** a classified finding renders its ids next to the title as `SQL injection [CWE-89]`, or `[CWE-798, CWE-532]` where several apply; an unclassified one renders exactly as before.
  - **Breaking:** `VerifiedFinding` and `FindingProse` each gain a public field, so an out-of-tree struct literal for either needs it. Hence the 0.34.0 MINOR bump.
