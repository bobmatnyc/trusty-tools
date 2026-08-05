Added

- Room-scoped recall entry points `recall_in_room` and `recall_deep_in_room`
  (ADR-0027 T7). `retrieve_l2` has enforced a room filter since #3274 but no
  recall entry point exposed it, so a room scope was unreachable from the
  recall path. L0/L1 stay unfiltered — they are the palace's always-on identity
  and essential grounding, not search results.
