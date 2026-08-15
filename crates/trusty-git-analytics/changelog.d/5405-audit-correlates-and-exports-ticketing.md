Added

- `tga audit`'s sweep now runs the commit ↔ board-item correlation pass as a
  ninth stage, immediately after collection. It previously ran only from `tga
  tui`, so an audit synced board data and never joined it to the commits it
  walked.
- The sweep writes `ticketing.json` beside `manifest.toml` and names it in the
  manifest's `[report]` section, carrying the correlation counts and the boards
  they came from to the due-diligence report. A run whose correlation stage
  failed writes no artifact and declares no key, so the report states the
  omission under Gaps & Caveats instead of rendering an empty section.
