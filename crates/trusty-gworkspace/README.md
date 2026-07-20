# trusty-gworkspace

Google Workspace MCP server for the Trusty suite — a Rust port of the
Python [`gworkspace-mcp`](https://pypi.org/project/gworkspace-mcp/) project.

Exposes 46 [Model Context Protocol](https://modelcontextprotocol.io/) tools
across Gmail, Calendar, Drive, Docs, Sheets, Slides, Tasks, and Accounts.
Authentication is fully native: the `trusty-gworkspace-mcp` binary runs the
OAuth consent flow itself and reads/writes `~/.gworkspace-mcp/tokens.json` —
no external CLI is required. The token file is wire-compatible with the
original Python `gworkspace-mcp`, so an existing token file is picked up
unchanged.

> **Breaking change (issue #2644):** the native binary was renamed from
> `gworkspace-mcp` to `trusty-gworkspace-mcp`. Previously the cargo-installed
> native binary and the legacy pipx-installed Python package both claimed the
> `gworkspace-mcp` name on `$PATH`, so which implementation actually launched
> depended on install order. Migration: update any `.mcp.json` /
> `~/.claude.json` entry's `"command"` field from `"gworkspace-mcp"` to
> `"trusty-gworkspace-mcp"`, then re-run `cargo install --path
> crates/trusty-gworkspace` (or `cargo install trusty-gworkspace`) to get the
> new binary name on `$PATH`. Token/config file paths (`~/.gworkspace-mcp/…`)
> are unchanged — existing accounts keep working without re-authenticating.

## Installation

```sh
cargo install --path crates/trusty-gworkspace
```

This installs the `trusty-gworkspace-mcp` binary into `~/.cargo/bin`. The same
binary is both the MCP stdio server (run with no subcommand) and the
onboarding CLI (`setup` / `doctor` / `accounts`).

## Authentication

### 1. Provide an OAuth client

`setup` needs a Google OAuth **client id + secret** (a "Desktop app" /
installed-app client from the Google Cloud console). Supply them either via
environment variables:

```sh
export GOOGLE_OAUTH_CLIENT_ID="…apps.googleusercontent.com"
export GOOGLE_OAUTH_CLIENT_SECRET="…"
```

…or by writing `~/.gworkspace-mcp/oauth_client.json`. Both a flat shape and the
console's downloaded shape are accepted:

```json
{ "client_id": "…apps.googleusercontent.com", "client_secret": "…" }
```

```json
{ "installed": { "client_id": "…", "client_secret": "…" } }
```

(`web` is accepted in place of `installed`.) Environment variables take
precedence over the file.

### 2. Authorize an account

```sh
trusty-gworkspace-mcp setup                       # authorize the default profile
trusty-gworkspace-mcp setup --profile work        # authorize a named profile
```

`setup` opens your browser to Google's consent screen, captures the redirect on
a loopback listener, and writes the minted token to `~/.gworkspace-mcp/tokens.json`.
Access tokens are then refreshed automatically by the server; you only re-run
`setup` when the refresh token itself is revoked or expired.

Default-profile rules: the first profile you authorize becomes the default; a
later `setup` of a *different* profile leaves the existing default untouched.
Use `--make-default` to switch it deliberately, or `--no-default` to guarantee
it is never changed.

### 3. Wire into Claude Code (or any MCP client)

```jsonc
// ~/.claude.json
{
  "mcpServers": {
    "gworkspace": {
      "command": "trusty-gworkspace-mcp"
    }
  }
}
```

The model can now call `search_gmail_messages`, `manage_events`,
`create_document`, `manage_slides`, etc. Pass `account = "<profile-name>"` to
any tool to target a non-default profile.

## Re-authenticating from within a session

When a refresh token dies mid-session (Google returns `invalid_grant`), tools
fail with an actionable message naming the exact fix, e.g.:

```
Google refresh token for profile 'work' is expired or revoked — re-authenticate
with: trusty-gworkspace-mcp setup --profile work (400 Bad Request invalid_grant: …)
```

You can run the fix without leaving the session:

```sh
! trusty-gworkspace-mcp setup --profile work        # interactive browser consent
! trusty-gworkspace-mcp setup --profile work --print-url   # headless: prints the URL
```

`--print-url` (alias `--no-browser`) skips the browser launch and prints the
full consent URL for you to open manually — the loopback callback still binds
and captures the redirect exactly as the interactive path does. This is the
path to use from a headless or in-session context where no browser can be
spawned for you.

## Diagnosing token health

`doctor` reports the static config checks **and** live-probes each stored
profile's refresh token, classifying it OK / DEAD / UNKNOWN:

```sh
trusty-gworkspace-mcp doctor
```

For any DEAD profile it prints the exact `trusty-gworkspace-mcp setup --profile <name>`
command to fix it. The probe is read-only (it never rewrites `tokens.json`),
bounded by a short per-profile timeout, and degrades to UNKNOWN when Google is
unreachable rather than failing the whole diagnostic.

## Managing accounts

```sh
trusty-gworkspace-mcp accounts list                 # show profiles + which is default
trusty-gworkspace-mcp accounts default work         # switch the default profile
trusty-gworkspace-mcp accounts remove work          # forget a local token (no revoke)
```

Removing the current default reassigns it to another remaining profile
(alphabetically next) rather than leaving no default at all.

The same operations are also available as MCP tools (issue #3503), so an
agent can manage accounts without shelling out to the CLI:

- `list_accounts` — read-only, as above.
- `set_default_account {name}` — same behavior as `accounts default`.
- `remove_account {name}` — same behavior as `accounts remove`, including the
  default-reassignment above; the response reports which profile (if any)
  became the new default.
- `add_account` — runs the native OAuth consent flow for a new (or re-auth of
  an existing) profile. This blocks the tool call for up to `timeout_secs`
  (default 60s, clamped to 10-90s) waiting for the user to complete consent in
  a browser, and always returns the consent URL in the response (whether it
  succeeded or timed out) so the calling agent can relay it and, on a
  time-out, simply call `add_account` again for a fresh URL. No partial or
  corrupt token is ever written on a time-out or abandoned consent.

## Configuration

| Env var                       | Purpose                                                        |
|-------------------------------|----------------------------------------------------------------|
| `GOOGLE_OAUTH_CLIENT_ID`      | OAuth client ID (required to run `setup` and to refresh)       |
| `GOOGLE_OAUTH_CLIENT_SECRET`  | OAuth client secret (required to run `setup` and to refresh)   |
| `RUST_LOG`                    | Standard `tracing` filter (e.g. `trusty_gworkspace=debug`)     |

### Token file shape

`~/.gworkspace-mcp/tokens.json` is a **flat JSON object mapping profile name →
stored token** (created 0600 on Unix). Multi-account support is any number of
named keys in this map:

```json
{
  "gworkspace-mcp": {
    "version": 1,
    "metadata": {
      "service_name": "gworkspace-mcp",
      "provider": "google",
      "created_at": "2026-01-01T00:00:00Z",
      "last_refreshed": null,
      "email": "user@example.com",
      "is_default": true
    },
    "token": {
      "access_token": "ya29.…",
      "refresh_token": "1//…",
      "expires_at": "2026-01-01T01:00:00Z",
      "scopes": ["openid", "https://www.googleapis.com/auth/gmail.modify"],
      "token_type": "Bearer"
    }
  }
}
```

A project-level `./.gworkspace-mcp/tokens.json` overrides the user-level file
per profile (two-tier lookup) — useful for per-project accounts.

## Tool Surface

46 tools, grouped by service:

- **Accounts:** `list_accounts`, `set_default_account`, `remove_account`,
  `add_account`
- **Calendar:** `manage_calendars`, `manage_events`, `query_free_busy`
- **Gmail:** `search_gmail_messages`, `get_gmail_message_content`,
  `download_gmail_attachment`, `list_message_attachments`, `compose_email`,
  `modify_gmail_messages`, `format_email_content`, `manage_gmail_labels`,
  `manage_gmail_filters`, `manage_gmail_settings`
- **Drive:** `list_drive_contents`, `search_drive_files`,
  `get_drive_file_content`, `list_shared_drives`, `manage_drive_file`,
  `manage_file_permissions`
- **Docs:** `create_document`, `append_to_document`, `get_document`,
  `get_document_structure`, `replace_text_in_document`,
  `insert_text_in_document`, `delete_range_in_document`,
  `manage_document_comments`, `format_document_range`, `set_document_style`,
  `insert_table_in_document`, `find_tables_in_document`,
  `manage_table_structure`
- **Sheets:** `get_spreadsheet`, `manage_spreadsheet`, `modify_sheet_values`,
  `format_sheet`
- **Slides:** `get_slides`, `manage_slides`, `add_slide_content`
- **Tasks:** `manage_task_lists`, `manage_tasks`

The authoritative list with JSON Schemas is in
[`src/tools.rs`](src/tools.rs).

## Architecture

Two layers, deliberately decoupled:

- **`api::`** — pure Google Workspace API client. `BaseClient` wraps
  `reqwest::Client`, resolves tokens via `TokenStorage`, and refreshes on
  401 via `OAuthManager`. Per-product service modules
  (`api::services::gmail`, `api::services::drive`, ...) hold the
  request-building logic and return `serde_json::Value`.
- **`server`** — MCP JSON-RPC dispatch. `handle_message` routes
  `initialize`, `tools/list`, and `tools/call` to handlers; `run_stdio`
  wires it into `trusty_mcp_core::run_stdio_loop`.

Every service function shares the signature
`async fn(&BaseClient, serde_json::Value) -> anyhow::Result<Value>`, so
adding a new tool means: write the function, add a `match` arm in
`server::handle_tool_call`, and append the JSON Schema in
`tools::tool_list_response`.

## Design Notes

- **Token format compatibility.** Wire-compatible with the original Python
  `gworkspace-mcp` token file, so an existing `tokens.json` is read unchanged.
  See [`src/api/auth/models.rs`](src/api/auth/models.rs).
- **Native interactive OAuth.** The authorization-code + PKCE consent flow is
  implemented in Rust (`src/api/auth/oauth/`): `setup` binds a loopback
  redirect listener, opens the browser (or, with `--print-url`, prints the URL
  for headless use), and mints the token itself. Access-token refresh is also
  native; no external CLI is involved.
- **Two-tier token lookup.** `./.gworkspace-mcp/tokens.json` overrides
  `~/.gworkspace-mcp/tokens.json` — useful for per-project profiles.
- **Concurrency-safe token writes (issue #3502).** Every read-modify-write of
  `tokens.json` (refresh, consent persistence, account default/remove) goes
  through `TokenStorage::update`, which serialises callers in-process and
  holds an advisory cross-process lock on a sidecar `tokens.json.lock` file
  for the duration. Two profiles refreshing at the same moment — in the same
  or different processes — can no longer silently lose one write.
- **Errors as data.** Tool failures return `{"error": "..."}` inside the
  MCP `content` envelope rather than JSON-RPC framing errors, so the
  model gets actionable feedback.
- **`add_account`'s blocking design (issue #3503).** Authorizing an account
  needs a human to finish consent in a browser, which an MCP tool call can't
  do end-to-end on its own. Rather than adding a background task + a separate
  poll/status tool, `add_account` reuses the existing consent flow verbatim
  with a short, bounded timeout (default 60s) and always returns the consent
  URL in the response, success or time-out. This was a deliberate choice to
  avoid new state-management machinery; it does mean the tool call itself
  blocks for up to the timeout, so clients with a shorter built-in call
  timeout may see it fail even though retrying is always safe (no token is
  persisted until the exchange fully succeeds).
- **Local filesystem access is intentionally broad.** `compose_email`
  attachments (`path`/`local_path`), `manage_drive_file`'s `upload` action
  (`local_path`), and `get_drive_file_content`'s `save_path` all read or
  write arbitrary local paths the calling agent supplies. There is no path
  confinement — an LLM-driven agent processing untrusted content (e.g. a
  malicious email body instructing it to attach `~/.ssh/id_rsa`) can read or
  exfiltrate any file the process's user can access. Treat this server the
  same as any other tool granting a model direct filesystem read/write.

## Testing

```sh
cargo test -p trusty-gworkspace
```

Covers auth-model deserialisation, the `tools/list` shape, and the MCP
handshake. Live Google API calls are out of scope for the test suite.

## License

Licensed under the [MIT License](https://opensource.org/licenses/MIT).

## Repository

<https://github.com/bobmatnyc/trusty-common>
