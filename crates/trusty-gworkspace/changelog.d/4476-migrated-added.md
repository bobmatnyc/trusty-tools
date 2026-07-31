Added

- New Gmail connector-layer functions for trusty-agents' eventstream
  listener (#3820): `api::services::gmail::history::get_gmail_profile`
  (bootstraps a `historyId` cursor) and `list_gmail_history` (incremental
  `users.history.list` polling, optionally scoped to one label). Called as
  direct library functions, not through the MCP tool-dispatch path — no new
  MCP tools, scopes, or wire surface; existing OAuth scopes already cover
  both calls.
- Multi-account management is now exposed as MCP tools, not just the CLI
  (issue #3503): `set_default_account {name}`, `remove_account {name}`, and
  `add_account` (runs the native OAuth consent flow for a new or re-auth
  profile — see the README's "Managing accounts" section for the blocking-call
  design and its tradeoffs). `list_accounts` is unchanged.
- Per-profile OAuth-client support (issue #3518, follow-up to #3502/#3503):
  each account profile can now authorize (and forever after refresh) against
  its OWN OAuth client — e.g. a per-domain "Internal" Google Workspace app —
  instead of one shared global client. `setup --oauth-client <path>` and
  `add_account`'s new `oauth_client_path` argument persist a profile's client
  to `~/.gworkspace-mcp/clients/<profile>.json` (0600); it is reused
  automatically on every subsequent refresh. Profiles with no per-profile
  client keep using the global `oauth_client.json`/env vars exactly as
  before — no migration needed. `accounts list` / `list_accounts` / `doctor`
  now show which client each profile uses.
