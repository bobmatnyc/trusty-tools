Fixed

Membership-style knowledge-graph facts are recorded correctly (trusty-common
#4810). A predicate that names several things — a room's drawers, a project's
dependencies — used to keep only the most recently asserted object; all of them
are now retained, and `/kg/graph`, `kg_gaps`, and neighbour expansion report the
real edge set instead of one edge per `(subject, predicate)`.

Expect `kg_count` totals to RISE on existing palaces after the one-time
migration at first open. That is the previously-hidden data becoming visible,
not a regression. Alias and convention predicates (`is_alias_for`,
`has_convention`, `is_fact`, `is_shorthand_for`) are unaffected — they remain
single-valued, so prompt-fact injection does not grow.
