Added

- **`kg_retract_triple` MCP tool — the inverse of `kg_assert`.** A triple
  asserted over MCP could not be taken back over MCP: `remove_prompt_fact` is
  scoped to hot predicates and closes the whole `(subject, predicate)` pair,
  and re-asserting adds an object rather than replacing it for any predicate
  outside the functional set. The tool takes the full
  `(subject, predicate, object)` key, closes exactly that triple, and leaves
  every sibling object at the pair active. It returns
  `{palace, subject, predicate, object, closed, retracted}` — `closed` is 1 for
  a retraction and 0 when nothing matched, so a miss is a legible no-op rather
  than a silent one, and a repeat call is safe. `object` is required: omitting
  it is an error, not a pair-wide retraction. Retracting a hot-predicate triple
  rebuilds the prompt cache so the fact stops being injected.
