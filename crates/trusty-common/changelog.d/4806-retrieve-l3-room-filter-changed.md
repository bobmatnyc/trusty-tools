Changed

- `retrieve_l3` takes an optional `room_filter`, matching `retrieve_l2`
  (ADR-0027 T7). Deep recall was the one retrieval path a room scope could not
  reach, so a caller narrowing to one room silently got every room back. Callers
  pass `None` for the previous behaviour, which is byte-identical; when a filter
  is set the search over-fetches so filtered-out neighbours do not eat the
  `top_k` budget.
