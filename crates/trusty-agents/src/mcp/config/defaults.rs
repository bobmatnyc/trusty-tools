//! The default `~/.trusty-agents/config.toml` literal.
//!
//! Why: Stored as a single literal so the on-disk shape is identical to what
//! callers see when they load the file, and so `default_config()` can parse it
//! to materialize the documented in-memory defaults. Pulled into its own file
//! because the literal alone is ~290 lines.
//! What: `DEFAULT_CONFIG_TOML` — the full commented default config.
//! Test: `default_config_is_valid_toml` and friends in `config::tests`.

/// Default config TOML written when `~/.trusty-agents/config.toml` is absent.
///
/// Why: Stored as a literal so the on-disk shape is identical to what callers
/// see when they load the file; comments document intent for human editors.
pub(super) const DEFAULT_CONFIG_TOML: &str = r#"# trusty-agents global configuration
# ~/.trusty-agents/config.toml

[mcp]
# Agent roles that receive MCP tool descriptions in their system prompt
inject_for_roles = ["ctrl", "pm", "research", "observe"]

# Remote and service-tier MCPs — these are external platforms agents can reference.
# Local native integrations (kuzu-memory, mcp-vector-search) are handled by the
# harness directly and do not appear here.

[[mcp.services]]
name = "gworkspace-mcp"
description = "Google Workspace — Gmail, Calendar, Drive, Docs, Sheets, Tasks"
command = "gworkspace-mcp"
args = ["mcp"]
transport = "stdio"
enabled = true

[[mcp.services.tools]]
name = "gmail_search"
description = "Search Gmail messages by query"

[[mcp.services.tools]]
name = "gmail_send"
description = "Send an email via Gmail"

[[mcp.services.tools]]
name = "gmail_read"
description = "Read a Gmail message by ID"

[[mcp.services.tools]]
name = "gmail_list"
description = "List Gmail messages with optional filters"

[[mcp.services.tools]]
name = "calendar_list"
description = "List Google Calendar events"

[[mcp.services.tools]]
name = "calendar_create"
description = "Create a Google Calendar event"

[[mcp.services.tools]]
name = "calendar_update"
description = "Update an existing calendar event"

[[mcp.services.tools]]
name = "drive_search"
description = "Search Google Drive files"

[[mcp.services.tools]]
name = "drive_read"
description = "Read a Google Drive file"

[[mcp.services.tools]]
name = "drive_create"
description = "Create a file in Google Drive"

[[mcp.services.tools]]
name = "docs_read"
description = "Read a Google Doc"

[[mcp.services.tools]]
name = "docs_create"
description = "Create a new Google Doc"

[[mcp.services.tools]]
name = "docs_update"
description = "Update content in a Google Doc"

[[mcp.services.tools]]
name = "sheets_read"
description = "Read data from Google Sheets"

[[mcp.services.tools]]
name = "sheets_update"
description = "Write data to Google Sheets"

[[mcp.services.tools]]
name = "tasks_list"
description = "List Google Tasks"

[[mcp.services.tools]]
name = "tasks_create"
description = "Create a Google Task"

[[mcp.services.tools]]
name = "tasks_complete"
description = "Mark a Google Task as complete"

[[mcp.services]]
name = "slack-user-proxy"
description = "Slack messaging — send messages, read channels, search"
command = "slack-user-proxy"
args = []
transport = "stdio"
enabled = false

[[mcp.services.tools]]
name = "slack_post"
description = "Post a message to a Slack channel"

[[mcp.services.tools]]
name = "slack_search"
description = "Search Slack messages"

[[mcp.services.tools]]
name = "slack_read"
description = "Read messages from a Slack channel"

# Granola — meeting notes, transcripts, and action items (#256).
# Enabled by default; harmless when the binary is absent (the harness
# logs and continues). Used heavily by the personal-assistant and
# cto-assistant for meeting recall.
[[mcp.services]]
name = "granola-notes"
description = "Granola meeting notes and transcripts"
command = "/opt/homebrew/bin/granola-mcp"
args = []
transport = "stdio"
enabled = true

[[mcp.services.tools]]
name = "granola_search"
description = "Search Granola meeting notes and transcripts"

[[mcp.services.tools]]
name = "granola_get"
description = "Retrieve a specific Granola meeting note"

[[mcp.services.tools]]
name = "granola_list_recent"
description = "List recent Granola meetings"

# Duetto org memory service — Duetto-internal HTTP MCP (#256).
# Disabled by default since it's only reachable on Duetto infrastructure.
# Enable with `mcp_enable duetto-memory` when on a Duetto-connected machine.
[[mcp.services]]
name = "duetto-memory"
description = "Duetto org memory service (Duetto infra only)"
url = "https://mcp-services.dev.duettosystems.com/memory/mcp"
transport = "http"
enabled = false

# GitHub identities for the ticketing agent (#243).
# Each identity points to env vars holding a token + default repo, so
# secrets stay out of this file. Set `default_identity` to choose which
# identity is used when no override is provided.
#
# Example:
# [github]
# default_identity = "personal"
#
# [[github.identities]]
# name = "personal"
# token_env = "GITHUB_TOKEN"
# repo_env = "GITHUB_REPO"
#
# [[github.identities]]
# name = "work"
# token_env = "GITHUB_TOKEN_WORK"
# repo_env = "GITHUB_REPO_WORK"

# Native git tool configuration (#247).
# Controls which agent roles get the git_* tools (status, log, branches,
# commit, push, pull, fetch, stash, etc.). Read operations use libgit2;
# write operations shell out to `git` to preserve hooks and signing.
[git]
available_for_roles = ["ctrl", "pm", "research", "observe"]
confirm_writes = false
default_branch = "main"

# Local Ollama fast-path (#319).
# When enabled, qualifying ctrl turns (TM status queries, simple chat) are
# routed to a locally-running ollama instance instead of the remote model.
# Enabled by default (#345). Toggle with `/local off` or set `enabled = false`
# below. Requires `ollama serve` and a pulled model matching `model`.
[local_inference]
enabled = true
model = "ollama/qwen3:30b"
fallback_on_error = true
ollama_host = "http://localhost:11434"
max_tokens = 2048

# OpenRPC (https://spec.open-rpc.org/) over stdio tool registry (#453, #455).
# Declares external JSON-RPC 2.0 endpoints (driver = "direct") that advertise
# tools via `rpc.discover`. The `direct` driver spawns a subprocess and
# speaks JSON-RPC 2.0 over its stdin/stdout (NDJSON; one JSON object per
# line, JSON array for batch). Endpoints below are DISABLED by default —
# flip `enabled = true` once the corresponding binary supports an OpenRPC
# stdio mode (e.g. via a `--rpc` flag). See
# docs/trusty-agents/research/openrpc-trusty-contract.md for the wire format.
[tool_registry]
scope_enforcement = "deny"

# trusty-memory — recall/remember/forget over JSON-RPC 2.0 stdio.
# Disabled until the trusty-memory binary supports `--rpc` mode (where it
# reads OpenRPC requests from stdin and writes responses to stdout).
[[tool_registry.endpoints]]
name = "trusty-memory"
driver = "direct"
description = "Trusty memory service — recall/remember/forget"
command = "trusty-memory"
args = ["--rpc"]
enabled = false
scopes = ["memory.read", "memory.write"]
discovery_ttl_secs = 0
eager_discovery = true

[tool_registry.endpoints.transport]
timeout_ms = 5000

# trusty-search — semantic/keyword code search over JSON-RPC 2.0 stdio.
# Disabled until the trusty-search binary supports `--rpc` mode.
[[tool_registry.endpoints]]
name = "trusty-search"
driver = "direct"
description = "Trusty search service — semantic + keyword code search"
command = "trusty-search"
args = ["--rpc"]
enabled = false
scopes = ["search.read"]
discovery_ttl_secs = 0
eager_discovery = true

[tool_registry.endpoints.transport]
timeout_ms = 5000

# gworkspace — Google Workspace (Gmail, Calendar, Drive, Docs, Sheets, Tasks)
# via JSON-RPC 2.0 stdio. The `gworkspace-mcp` binary (from the trusty-common
# workspace, crate `trusty-gworkspace`) exposes an OpenRPC 1.3.2 manifest via
# `rpc.discover` and advertises Google OAuth scopes per tool through the
# `x-google-scopes` extension. Disabled by default — flip `enabled = true`
# after authenticating (`gworkspace-mcp auth login`) on a machine with the
# binary on $PATH. See docs/trusty-agents/research/openrpc-trusty-contract.md.
[[tool_registry.endpoints]]
name = "gworkspace"
driver = "direct"
description = "Google Workspace — Gmail, Calendar, Drive, Docs, Sheets, Tasks"
command = "gworkspace-mcp"
args = []
enabled = false
scopes = [
    "google.gmail.*",
    "google.calendar.*",
    "google.drive.*",
    "google.docs.*",
    "google.sheets.*",
    "google.tasks.*",
]
discovery_ttl_secs = 0
eager_discovery = true

[tool_registry.endpoints.transport]
timeout_ms = 5000

# tickets-mcp — unified ticketing MCP server (GitHub Issues, JIRA, Linear)
# via JSON-RPC 2.0 stdio. Disabled by default; flip `enabled = true` once
# the `tickets-mcp` binary is on $PATH.
[[tool_registry.endpoints]]
name = "tickets-mcp"
driver = "direct"
description = "Unified ticketing MCP server — GitHub Issues, JIRA, Linear"
command = "tickets-mcp"
args = []
enabled = false
scopes = ["ticketing.*"]
discovery_ttl_secs = 0
eager_discovery = true

[tool_registry.endpoints.transport]
timeout_ms = 5000

[[tool_registry.endpoints]]
name = "commons-ticketing"
driver = "direct"
description = "Commons ticketing — create/update/close GitHub issues and PRs via OpenRPC stdio"
command = "commons-ticketing"
args = ["--rpc"]
enabled = false
scopes = ["ticketing.read", "ticketing.write", "ticketing.admin"]
discovery_ttl_secs = 300
eager_discovery = true

[tool_registry.endpoints.transport]
timeout_ms = 5000
"#;
