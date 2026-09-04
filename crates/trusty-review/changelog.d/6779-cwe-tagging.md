Added

- Investigation findings can now carry a CWE weakness class, so a consumer counting ISO-5055-style structural flaws no longer has to infer one from prose ([#6779](https://github.com/bobmatnyc/trusty-tools/issues/6779)).
  - **Schema:** the investigation response schema declares an optional `cwe_id` and names the weakness classes the crate reads back, so the model picks from a known vocabulary rather than inventing a spelling.
  - **Ingestion:** `report::investigate::cwe::resolve` admits a well-formed `CWE-<number>` id (upper-casing it) or a class NAME from one table of 12 weakness classes. Anything else — a malformed id, an unknown class, an empty field — resolves to nothing and is dropped; a finding is never rejected for it, and an id is never repaired into a neighbouring one.
  - **Serialisation:** `cwe_id` is skipped when absent, so a finding with no identifiable weakness class carries no field at all in `investigation.json`. A GREEN finding names a strength, so it carries no weakness class either.
  - **Report:** a classified finding renders its id next to the title as `SQL injection [CWE-89]`; an unclassified one renders exactly as before.
