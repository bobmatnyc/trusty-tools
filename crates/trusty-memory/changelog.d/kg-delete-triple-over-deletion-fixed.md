Fixed

- `DELETE /api/v1/palaces/{id}/kg/triples/{triple_id}` closes the one triple named, not every object at its `(subject, predicate)` pair.
  - The id encoded only `subject + "\0" + predicate` and the service called the pair-level retract, so deleting `alpha is thing-a` also closed `alpha is thing-b`. Retraction is a soft close, so triples lost this way are still readable through `dump_all_triples`.
- Retracting a hot-predicate triple over HTTP rebuilds the prompt cache, matching the `kg_retract_triple` MCP tool. Previously a retracted Tier S fact kept being injected until the next write.
