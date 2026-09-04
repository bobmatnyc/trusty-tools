Added

- `trusty-search quantize` (and `POST /indexes/:id/quantize`) re-encodes an existing index's vectors at a different scalar precision in place. This is the only way an index built before the `f16` default becomes quantized, since a forced reindex upserts into the store built at warm-boot and therefore re-embeds at the old precision. It reports before it writes (`--dry-run` names the index, root, chunk count, vector count and snapshot bytes), prompts unless `--yes`, is a no-op when the index already holds the target precision, and refuses while a reindex is in flight.
- `GET /indexes/:id/status` reports `semantic_coverage.vector_quant`: the precision the LIVE index holds, which for any index built before the default flip differs from what `TRUSTY_VECTOR_QUANT` would suggest.
