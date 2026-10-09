Added
- `memory_list` reports a read-only `fact_key` on every drawer: the ADR-0028
  Tier C slot the drawer holds, or `null` when it holds none. A drawer that a
  later write to the same slot displaced reads `null`, so a client can resolve
  a slot to its one current drawer id from the listing (#9340).
