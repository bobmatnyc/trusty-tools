Fixed

- vercel-ops no longer audits environment variables with `vercel env ls --json` / `--format json`, which prints values the redacted table view hides; audits now use the table form only, and a genuinely needed value routes through the secrets model (epic #7517) instead of the agent's own context (Refs #7500).
