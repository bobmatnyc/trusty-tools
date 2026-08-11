Breaking

- `triple_id` in `DELETE /api/v1/palaces/{id}/kg/triples/{triple_id}` is now the base64url encoding of `subject + "\0" + predicate + "\0" + object`. An id in the old two-field form is rejected with `400` and a message naming the new format — it cannot identify a single triple, and accepting it is what closed every object at the pair. Ids are derived from the fields, never persisted or returned by any endpoint, so a caller rebuilds one by adding the object.
- `MemoryService::kg_retract_triple` takes an `object` and returns the number of rows closed (`usize`) instead of `bool`.
