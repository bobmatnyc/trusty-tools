Fixed

- `PalaceRow` now distinguishes an unknown count from a zero one ([#4682](https://github.com/bobmatnyc/trusty-tools/issues/4682))
  - `GET /api/v1/palaces` returns `cached: false` with all counts zeroed for any palace whose handle is not resident (2,180 of 2,183 on a live daemon); those zeros mean *unknown*, not *empty*
  - rows parsed from an uncached entry are flagged `counts_unknown`, and the new `vectors()` / `drawers()` / `kg_triples()` / `nodes()` / `edges()` accessors return `Option<u64>` so a renderer cannot print a placeholder as a measurement
  - the dashboard memory panel, the `trusty-memory monitor palaces` CLI, and the `/ui` web dashboard render `—` for those counts and sum only measured ones
  - a daemon that omits `cached` entirely (pre-#4640) is still trusted, so counts do not regress to `—` against an older daemon
  - `MemoryClient::fetch_palace()` hits `GET /api/v1/palaces/{id}` to fetch live counts for a single palace, alongside the new `parse_palace_detail()` and `format_opt_count()` helpers
