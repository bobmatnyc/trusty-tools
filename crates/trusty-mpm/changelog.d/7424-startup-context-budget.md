Added

- Startup-context budget (#7424). Every managed session now records what its
  FIRST assistant turn re-sent — `input + cache_creation + cache_read`, read
  from the session's own transcript — once, beside the savings ledger under
  `~/.trusty-mpm/usage/`, keyed by Claude session id. `tm session ls` shows it
  per row in a new `START` column, and a new `tm doctor` check,
  `startup_context`, warns (never fails) when the median or the latest reading
  for the current project reaches a ceiling configured as
  `startup_context.ceiling_tokens` in `~/.trusty-tools/trusty-mpm/config.yaml`,
  default 50,000 tokens over the newest 10 sessions. The check opens no
  transcript, so it cannot read another project's session data. A CI-side gate,
  `scripts/check_context_budget.sh`, weighs `CLAUDE.md`, the framework
  instruction sections and the active output style against a committed baseline
  and fails when the total grows more than 5%, or one file more than 10%.
