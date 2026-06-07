# trusty-gworkspace

Google Workspace MCP server for the Trusty suite — a Rust port of the
Python [`gworkspace-mcp`](https://pypi.org/project/gworkspace-mcp/) project.

Exposes 43 [Model Context Protocol](https://modelcontextprotocol.io/) tools
across Gmail, Calendar, Drive, Docs, Sheets, Slides, Tasks, and Accounts.
Reads OAuth tokens from `~/.gworkspace-mcp/tokens.json`, so a user who
already authenticated with the Python CLI can switch to this Rust binary
without re-auth.

## Installation

```sh
cargo install --path crates/trusty-gworkspace
```

This installs the `gworkspace-mcp` binary into `~/.cargo/bin`.

## Quick Start

1. **Authenticate** using the Python CLI (one-time):

   ```sh
   pipx install gworkspace-mcp
   gworkspace-mcp auth
   ```

   This writes `~/.gworkspace-mcp/tokens.json`. Token refresh is handled
   automatically by `trusty-gworkspace` afterward (via `GOOGLE_CLIENT_ID` /
   `GOOGLE_CLIENT_SECRET` env vars when refresh is needed).

2. **Wire into Claude Code** (or any MCP client):

   ```jsonc
   // ~/.claude.json
   {
     "mcpServers": {
       "gworkspace": {
         "command": "gworkspace-mcp"
       }
     }
   }
   ```

3. **Use from Claude Code:** the model can now call `search_gmail_messages`,
   `manage_events`, `create_document`, `manage_slides`, etc.

## Configuration

| Env var                       | Purpose                                                    |
|-------------------------------|------------------------------------------------------------|
| `GOOGLE_CLIENT_ID`            | OAuth client ID (only required when refreshing tokens)     |
| `GOOGLE_CLIENT_SECRET`        | OAuth client secret (only required when refreshing tokens) |
| `GWORKSPACE_TOKENS_DIR`       | Override token storage directory (default `~/.gworkspace-mcp`) |
| `RUST_LOG`                    | Standard `tracing` filter (e.g. `trusty_gworkspace=debug`) |

Token files use the same format as the Python project:

```json
{
  "version": 1,
  "metadata": { "default": "gworkspace-mcp" },
  "tokens": {
    "gworkspace-mcp": {
      "metadata": { "email": "user@example.com", "is_default": true },
      "token": { "access_token": "...", "refresh_token": "...", "expires_at": "..." }
    }
  }
}
```

Multi-account support: any number of named profiles in `tokens.json`. Pass
`account = "<profile-name>"` to any tool to switch.

## Tool Surface

43 tools, grouped by service:

- **Accounts:** `list_accounts`
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

- **Token format compatibility.** Wire-compatible with the Python project
  so a single auth flow serves both implementations. See
  [`src/api/auth/models.rs`](src/api/auth/models.rs).
- **No interactive OAuth.** Refresh is implemented in Rust; the initial
  consent flow is delegated to the Python CLI. This keeps the binary
  headless and CI-friendly.
- **Two-tier token lookup.** `./.gworkspace-mcp/tokens.json` overrides
  `~/.gworkspace-mcp/tokens.json` — useful for per-project profiles.
- **Errors as data.** Tool failures return `{"error": "..."}` inside the
  MCP `content` envelope rather than JSON-RPC framing errors, so the
  model gets actionable feedback.

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
