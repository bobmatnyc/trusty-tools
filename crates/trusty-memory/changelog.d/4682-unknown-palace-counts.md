Fixed

- palace counts that are UNKNOWN no longer render as a bare `0` (closes [#4682](https://github.com/bobmatnyc/trusty-tools/issues/4682))
  - the /ui Palaces view showed `0 wings / 0 drawers / 0 vectors` directly above a "Drawers (1)" list, because the header badges came from the peek-based `GET /api/v1/palaces` while the drawer list came from a route that opens the palace
  - expanding a palace now also fetches `GET /api/v1/palaces/{id}` (~0.1s) and merges its live counts into that row; `api.getPalace()` had existed and been dead code
  - `monitor palaces <id>` reads `GET /api/v1/palaces/{id}` instead of filtering the bulk list, so its counts no longer depend on whether the palace happened to be LRU-resident
  - wherever the daemon reports `cached: false`, the UI and CLI render `—` (JSON: `null`) rather than `0`
  - the peek-based list route from #4640/#4637 is unchanged — this was entirely caller-side
