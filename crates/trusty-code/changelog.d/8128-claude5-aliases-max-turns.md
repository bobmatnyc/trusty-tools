Changed
- The `opus` / `sonnet` / `haiku` model aliases now resolve to the current
  Claude tiers — `anthropic/claude-opus-5`, `anthropic/claude-sonnet-5` and
  `anthropic/claude-haiku-4.5` — and `DEFAULT_MODEL` follows `sonnet`.
- `tcode run-task` gains `--max-turns <N>` (env fallback `TCODE_MAX_TURNS`),
  overriding the top-level loop's built-in cap of 8 on both the daemon and
  `--legacy-in-process` paths. A value of 0 is rejected.
- Cost reporting carries explicit rows for the opus-5, sonnet-5 and haiku-4.5
  families, matched ahead of the older 4.x substring fallbacks, so
  `RunReport.cost_usd` prices those models at their published rates.
