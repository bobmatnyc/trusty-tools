# Changelog

All notable changes are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

---
## [Unreleased]

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
