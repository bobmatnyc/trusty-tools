Fixed

- `palace_reembed` no longer gives a false all-clear for drawers whose vector was overwritten by another drawer's (closes [#5005](https://github.com/bobmatnyc/trusty-tools/issues/5005))
  - it now returns `alias_audit` (`clean` | `aliased` | `unavailable`), `alias_audit_error`, `vector_key_rows`, `distinct_vector_ids`, `aliased`, and `aliased_ids` alongside `missing`; `missing` counts drawers with no vector key, and an aliased drawer HAS a key, so `missing: 0` was reported for a palace with four unretrievable drawers
  - a failed audit reports `unavailable` with null counts, never zeros — a deletion-bearing workflow must require `alias_audit == "clean"` as well as `missing == 0` ([#5000](https://github.com/bobmatnyc/trusty-tools/issues/5000) resolution item 3)
