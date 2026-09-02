Added
- `trusty-analyze version` subcommand, so `tctl doctor trusty-analyze
  --self-check` can spawn `version --json` instead of failing on a clap usage
  error. `--json` emits the DOC-1 capability-discovery envelope
  (`contract_version`, `tool_version`, `verbs`); without it, a one-line
  `trusty-analyze v<version>` (#6631).
