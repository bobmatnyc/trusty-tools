Added

- `trusty-review version [--json]`. `--json` emits the DOC-1 capability-discovery
  envelope (`contract_version`, `tool`, `tool_version`, `verbs`) that
  `tctl doctor --self-check trusty-review` spawns and parses. The subcommand did
  not exist, so clap exited 2 with a usage error and the probe reported
  `trusty-review version --json exited with exit status: 2` (#6913). It answers
  from the binary alone — no config file, no tokio runtime, no network.
