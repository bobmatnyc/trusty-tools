# trusty-gworkspace — Google Workspace Client Library

**Purpose**: OAuth-authenticated client library for Google Workspace APIs (Calendar, Tasks, Drive, Gmail).

**License**: Elastic License 2.0

## Design

- **OAuth 2.0**: Standard Google OAuth flow with refresh token management
- **API coverage**: Calendar (events, free/busy), Tasks, Drive (file search/copy), Gmail (list, search, read)
- **Async/await**: Fully async via reqwest and tokio
- **Error handling**: Structured errors with retry-friendly semantics
- **Rate limiting**: Built-in exponential backoff and quota tracking

## Services

### Calendar Service
- List events and busy periods for a user
- Create, update, delete calendar events
- Query free/busy across multiple calendars
- Time zone handling and date range queries

### Tasks Service
- List task lists
- Create, update, complete tasks
- Query by task list ID
- Deadline and note support

### Drive Service
- Search files by name or metadata
- Copy files with new titles
- List files in a folder
- Metadata queries (owner, modified time, permissions)

### Gmail Service
- List messages with filtering and pagination
- Search messages via query language
- Read message content (headers, body, attachments)
- Label management

## Configuration

Configured via:
- OAuth credentials (client ID, secret) from Google Cloud Console
- Refresh tokens from successful OAuth flow
- Environment variables for API keys (optional, for service account mode)

## Integration Points

- **tc-services**: High-level sync and query layer
- **trusty-mpm**: Context retrieval for agent workflows
- **Directory MCP**: Calendar and task syncing

## See Also

- `crates/trusty-gworkspace/README.md` for full API
- `crates/tc-services/README.md` for sync layer
