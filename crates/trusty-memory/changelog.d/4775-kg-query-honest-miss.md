Fixed

- **`kg_query` no longer reports an empty graph when only the subject is
  missing (issue #4775).** Any subject with no active triples got the hint
  "Knowledge graph is empty. Run kg_bootstrap …", including on a graph holding
  thousands of triples — the handler never consulted a whole-graph total, so it
  asserted something it could disprove, and sent callers to re-seed an
  already-seeded graph. The response now always carries `kg_triple_count` (the
  whole-graph active total, on hits and misses alike), and a miss adds a
  `graph_state` of `subject_not_found` or `graph_empty` with the matching hint —
  the `subject_not_found` hint names `kg_list_subjects` so the recovery step is
  a tool call rather than a second guess. A hit carries neither field, so their
  absence means the subject was found. The MCP tool schema is unchanged.
- The test that pinned the old behavior (`kg_query_emits_hint_when_palace_empty`)
  asserted the falsehood and passed: `palace_create` auto-bootstraps at least two
  triples, so the graph it called empty never was. It is replaced by one test per
  outcome, each establishing its own precondition.
