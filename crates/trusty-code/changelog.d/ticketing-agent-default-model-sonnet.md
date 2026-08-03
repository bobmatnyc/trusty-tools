Changed

- `ticketing` agent's default model tier is now `sonnet`, up from `haiku`,
  matching the trusty-mpm bundled default (kept byte-parity by
  `scripts/check_agent_assets.sh`). Duplicate-detection and scope-boundary
  judgement are judgement calls, not clerical ones.
