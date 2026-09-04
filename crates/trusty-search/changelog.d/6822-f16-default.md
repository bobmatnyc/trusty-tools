Changed

**Default behaviour change for newly built indexes.** `TRUSTY_VECTOR_QUANT` now
defaults to `f16` instead of `f32`: every index created from this version on
stores half-precision vectors, halving the vector bytes it holds in RAM and on
disk. Recall@10 measures 1.00 — the same as f32 — on the `ooc_quick_wins`
fixture's query set. Set `TRUSTY_VECTOR_QUANT=f32` to keep full precision;
`i8` is unchanged and stays opt-in. An empty value now means "unset" and
resolves to the default rather than to `f32`.

Existing indexes are untouched: usearch records the scalar kind in the snapshot
header and rebuilds the metric from it on every open, so opening an f32 index
under the new default reads it as f32 and rewrites no bytes.

Added

`trusty-search quantize` (and `POST /indexes/:id/quantize`) converts an existing
index's vectors to a different precision in place — the only way an index built
before this change becomes quantized, since a forced reindex upserts into the
store built at warm-boot and therefore re-embeds at the old precision. It
reports before it writes (`--dry-run` names the index, root, chunk count, vector
count and snapshot bytes), prompts unless `--yes`, is a no-op when the index is
already at the target, and refuses while a reindex is in flight.

`GET /indexes/:id/status` gained `semantic_coverage.vector_quant`: the precision
the LIVE index holds, which for any index built before this change differs from
what `TRUSTY_VECTOR_QUANT` would suggest.
