Fixed

- The knowledge-graph TRIPLES key now includes the object, so a subject can hold several objects under one predicate ([#4810](https://github.com/bobmatnyc/trusty-tools/issues/4810))
  - It previously keyed on `(subject, predicate)` alone, which meant `room:General --contains--> drawer:X` held ONE row no matter how many drawers the room had — every new member silently closed the last one.
  - Predicates listed in `kg_store::FUNCTIONAL_PREDICATES` (`is_alias_for`, `has_version`, …) keep the one-active-object rule; every other predicate is multi-valued.
  - `upsert_edge` in the in-memory adjacency was collapsing the same edges back on every palace open and now matches on target as well as predicate.
- **Breaking on-disk format.** Palaces are migrated in place at open, once, in a single redb transaction, after a size-verified copy of the database is written to `<kg>.redb.pre-4810.bak`.
  - A migrated palace cannot be read by an earlier version of this crate — downgrading below this version is unsafe.
  - A migration that fails is logged at `warn!` and retried on the next open; the palace still opens, but its triples do not appear in queries until it succeeds.
