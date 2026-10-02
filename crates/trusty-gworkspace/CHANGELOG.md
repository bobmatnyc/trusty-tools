# Changelog

All notable changes are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

---

## [0.3.0] — 2026-09-25

### Fixed

- A project-level token entry (`./.gworkspace-mcp/`) no longer silently shadows a newer or wider-scoped user-level entry for the same profile. When both stores hold a profile, a project entry naming a different account, or recording no account email, still wins; otherwise a strict scope superset wins, then the later consent time (refresh time only breaks a tie), and an exact tie keeps the project entry. Gmail filter writes no longer fail with 403 `ACCESS_TOKEN_SCOPE_INSUFFICIENT` after a re-consent made from another directory (#8539).
- Every differing shadow now logs one warning per profile naming the winning store and the reason, never a token value; the old warning fired only when the project entry had expired (#8539).
- Token refreshes and other writes go back to the store the entry was read from, instead of copying the merged view into the project store. A new consent made in a project directory is written to the project store, and also replaces the user-level entry when it is the same account; it never overwrites a different account's user-level credential (#8539).
- A refresh no longer overwrites a credential that a consent replaced while the refresh was in flight, and no longer reverts a default-profile change made while it ran (#8539).
- Token store files are written atomically (temp file, 0600, sync, rename), so a concurrent reader never sees a partial file (#8539).
- A token store that cannot be read or parsed no longer counts as empty for writes: updates fail and leave both files untouched. When a project store exists, either store being unreadable is now an error on read too, rather than silently serving the other store's entries, which may belong to another account or make the project's lone profile the default. With no project store, an unreadable user store warns once per file and error kind with only the path and parse position, never the file's content (#8539).
- Token writes no longer hang forever when the project and user store are the same file (running from `$HOME`, a symlinked `.gworkspace-mcp`, or hard-linked files) (#8539).
- Removing a profile deletes the other store's entry only when it names the same account, and a kept entry never becomes a second default. `accounts remove` and the `remove_account` tool report a kept user-level entry. When several profiles are marked default, the lowest profile name is used, so the choice is stable (#8539).
- `manage_tasks` accepts the flat task shape again (`title`, `notes`, `due`, `status`, `completed` at the top level) alongside the `task` (create) and `updates` (update) objects, and advertises those fields in its input schema. A flat create no longer fails with "missing 'task' object", and a flat update no longer sends an empty PATCH. When both shapes are given they are merged; a field set in both with different values is refused with an error naming the field, and a create or update with no task fields at all is refused ([#8629](https://github.com/bobmatnyc/trusty-tools/issues/8629))
- `manage_task_lists` `update` applies a flat `title` (merged with `updates` under the same rule) instead of sending an empty PATCH ([#8629](https://github.com/bobmatnyc/trusty-tools/issues/8629))
- `manage_calendars` `update` applies the flat `summary`, `description` and `time_zone` fields, and `manage_gmail_labels` `update` applies the flat `name`, `label_list_visibility`, `message_list_visibility` and `color` fields, instead of sending an empty PATCH. When both the flat fields and the `updates` object are given they are merged; a field set in both with different values is refused with an error naming the field, and an update with no fields at all is refused. Both tool schemas describe the flat fields, and `manage_gmail_labels` now advertises `color` ([#8632](https://github.com/bobmatnyc/trusty-tools/issues/8632))

### Changed

- **Breaking API change:** `RemoveOutcome` gains the public field `user_entry_remains` and is now `#[non_exhaustive]`, so code outside the crate can no longer build it with a struct literal. The next trusty-gworkspace release is 0.3.0 (#8539).

## [0.2.6] — 2026-09-25

### Added

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
- **`manage_drive_file` gains an `update` action** that replaces an existing
  Google Doc/Sheet/Slides file's content in place, keeping the file id, its
  share link, its permissions, and its revision history. It PATCHes the source
  bytes to `/upload/drive/v3/files/{id}?uploadType=media` (or `multipart` when
  `name` is supplied, so a rename rides along), and refuses any target that is
  not one of the three Google editor types with a structured error naming the
  actual `mimeType` — it never falls back to creating a new file
  ([#6685](https://github.com/bobmatnyc/trusty-tools/issues/6685))

### Fixed

- OAuth client-credential resolution/persistence (`client_store`) no longer
  silently falls back to the process's current working directory when the
  home directory cannot be determined; it now returns an explicit error
  instead of reading or writing credentials at a CWD-relative path (found in
  the 2026-08-19 self-audit).
- Updated OpenRPC doc comments that still named `open-mpm` as an orchestrator
  to say `trusty-agents` (renamed in #831).
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
- **The OAuth consent flow now requests `gmail.settings.basic`**, so
  `manage_gmail_filters` and `manage_gmail_settings` no longer fail with
  `403 ACCESS_TOKEN_SCOPE_INSUFFICIENT`. A profile authorized before this
  release keeps its old grant and must re-run
  `trusty-gworkspace-mcp setup --profile <name>` once
  ([#8539](https://github.com/bobmatnyc/trusty-tools/issues/8539))

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
- **MCP protocol primitives now come from the `trusty-mcp` crate instead of `trusty_common::mcp`** — imports move from `trusty_common::mcp::…` to `trusty_mcp::…`, and the `trusty-common/mcp` feature is replaced by a direct `trusty-mcp` dependency. `trusty-common` stays a dependency for `init_tracing`. No behaviour change (ADR-0040, [#5803](https://github.com/bobmatnyc/trusty-tools/issues/5803))

### Documentation

- Repaired every broken rustdoc intra-doc link in this crate and added
  `#![deny(rustdoc::broken_intra_doc_links)]` to its crate root(s), so a new
  one fails the build instead of shipping as dead text on docs.rs (#5744).

