# trusty-cto-db — CTO Database (SQLite)

**Purpose**: SQLite-backed persistent database for CTO context and organizational data.

**License**: Elastic License 2.0

## Design

- **Schema**: Versioned SQLite schema with support for employee records, organizational units, pod assignments, preferences, and role assignments
- **Migrations**: Automated schema migrations via `rusqlite` migration framework
- **ACID guarantees**: Full transactional support for data consistency
- **Connection pooling**: Thread-safe connection management for concurrent access

## Data Model

Key tables:
- `people`: Employee directory entries (name, email, department, pod, status)
- `org_units`: Google Workspace organizational unit hierarchy
- `roles`: Directory role assignments (admin, superadmin, readonly)
- `groups`: ELT/SELT/SLT group memberships
- `preferences`: User-scoped preference storage (dietary, scheduling, identity)
- `aliases`: Service usernames (GitHub, Jira, Slack, Linear, Confluence)

## Integration Points

- **tc-services**: Provides high-level queries and mutations over the raw schema
- **trusty-mpm**: Uses tc-services layer for directory lookups
- **directory MCP**: Frontend query interface over tc-services

## Configuration

Environment variables:
- `TRUSTY_CTO_DB_PATH`: SQLite database file location (default: `~/.trusty/cto.db`)

## See Also

- `crates/trusty-cto-db/README.md` for schema and query API
- `crates/tc-services/README.md` for service layer
