# Changelog

All notable changes are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

---
## [Unreleased]

### Added

- Multi-account management is now exposed as MCP tools, not just the CLI
  (issue #3503): `set_default_account {name}`, `remove_account {name}`, and
  `add_account` (runs the native OAuth consent flow for a new or re-auth
  profile — see the README's "Managing accounts" section for the blocking-call
  design and its tradeoffs). `list_accounts` is unchanged.

### Changed

- **BREAKING:** the native binary is renamed from `gworkspace-mcp` to
  `trusty-gworkspace-mcp` (`crates/trusty-gworkspace/Cargo.toml` `[[bin]]`),
  closing #2644. The cargo-installed native binary and the legacy
  pipx-installed Python `gworkspace-mcp` package previously shared one name on
  `$PATH`, so which implementation actually launched depended on install
  order. Migration: update any `.mcp.json` / `~/.claude.json` server entry's
  `"command"` from `"gworkspace-mcp"` to `"trusty-gworkspace-mcp"`, then
  re-run `cargo install --path crates/trusty-gworkspace`. Token/config file
  paths (`~/.gworkspace-mcp/…`) and the default profile name are unchanged —
  existing accounts keep working without re-authenticating.

### Fixed

- MCP server self-refresh in tm-managed sessions — `OAuthManager::from_env()`
  previously returned `None` (refresh disabled) whenever
  `GOOGLE_OAUTH_CLIENT_ID`/`GOOGLE_OAUTH_CLIENT_SECRET` env vars were absent,
  which is every tm-managed session; every such session's access token
  therefore 401'd against Google roughly an hour after each re-auth with no
  self-healing. `from_env()` now falls back to
  `~/.gworkspace-mcp/oauth_client.json` (the same source `setup`/`doctor`
  already read, via the shared `resolve_client_creds` helper) when the env
  vars are absent; env vars still win when present. Logs a warning (instead
  of failing silently) only when neither source yields credentials, closes
  #2946.
- stale project-level token shadowing now warns — `TokenStorage::load()`'s
  documented project-overrides-user precedence is unchanged, but when a
  project-level `<cwd>/.gworkspace-mcp/tokens.json` entry is expired while
  the user-level entry it shadows for the same profile is still valid, a
  structured warning now names both paths instead of silently serving the
  stale override forever. `load()` is on the per-request hot path
  (`BaseClient::get_access_token` calls it on every MCP tool invocation, plus
  again on 401 retry), so the warning is throttled to at most once per
  profile per process rather than repeating on every call.
- `tokens.json` read-modify-write is now guarded against concurrent writers
  losing each other's changes (issue #3502): two profiles refreshing at the
  same moment — in the same or different processes — could each `load()` the
  map before either `save()`d, silently dropping whichever write lost the
  race. Every mutation path (`OAuthManager::refresh`, consent persistence,
  `accounts default`/`accounts remove` and their new MCP-tool counterparts)
  now goes through `TokenStorage::update`, which serialises callers
  in-process and holds an advisory cross-process lock on a sidecar
  `tokens.json.lock` file for the duration.
- `accounts remove`/`remove_account` no longer orphans the default profile
  (issue #3502): removing the current default now deterministically
  reassigns it to another remaining profile (the alphabetically next one)
  instead of leaving zero default entries, which previously broke
  `BaseClient::resolve_stored`'s default-profile fallback for every
  subsequent call with no explicit `account`.
