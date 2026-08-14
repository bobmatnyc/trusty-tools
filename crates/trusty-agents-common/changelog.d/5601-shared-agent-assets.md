Added

- `agent_assets` — the agent-asset roster now lives here as one physical `.md`
  per agent, embedded once and shared by `trusty-mpm` and `trusty-code`. Exposes
  42 named `pub const &str` items, the filename-keyed `AGENT_ASSETS` table for
  consumers that compose `extends:` chains in memory, and `AGENT_ASSETS_DIR` for
  those that compose from a directory. Both crates previously shipped their own
  byte-identical copy of 30 of these files, kept in step by a CI diff that could
  only report drift after it landed.
