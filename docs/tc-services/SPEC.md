# tc-services — Service Layer Adapters

**Purpose**: High-level service adapters for CTO database, Granola API, and Google Workspace clients.

**License**: Elastic License 2.0

## Service Adapters

### CTO Database Service
- Query interface over trusty-cto-db (people, org units, roles, preferences, aliases)
- Cached lookups for frequent queries
- Transaction support for atomic updates
- Used by directory MCP and MPM for user/org context

### Granola Integration
- Sync historical hiring and people movement data
- Provide enrichment signals to CTO database
- Track org changes over time

### Google Workspace Integration
- Calendar sync (`trusty-gworkspace`)
- Task management and Gmail integration
- Directory sync from Google Workspace org units

## Architecture

- **Abstraction layer**: Service traits decouple callers from underlying storage
- **Caching**: LRU caches for frequently accessed data
- **Error handling**: Structured errors with retry-friendly semantics
- **Async runtime**: tokio-based async I/O

## Configuration

Configured via:
- Environment variables for database paths
- Credentials for third-party APIs (Google Workspace OAuth, Granola tokens)

## See Also

- `crates/tc-services/README.md` for API details
- `crates/trusty-cto-db/README.md` for database schema
- `crates/trusty-gworkspace/README.md` for Google Workspace integration
