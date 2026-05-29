# trusty-cto-db

SQLite-backed persistent database for CTO context, employee directory, organizational structure, and user preferences.

**License**: Elastic License 2.0

## Purpose

`trusty-cto-db` provides the canonical persistent layer for:
- Employee directory (people, titles, departments, managers)
- Organizational unit hierarchy (Google Workspace org units)
- Pod and team assignments
- Role assignments (admin, superadmin, readonly)
- Group memberships (ELT, SELT, SLT)
- User preferences (dietary, scheduling, identity, contact methods)
- Service aliases (GitHub, Jira, Slack, Linear, Confluence usernames)

## Architecture

### Schema

Core tables (see `migrations/` for full schema):
- `people`: Employee records with status, employment type, department, pod, manager chain
- `org_units`: Google Workspace org unit hierarchy
- `roles`: Directory role assignments (admin, superadmin, readonly, and grants)
- `groups`: ELT/SELT/SLT group memberships
- `preferences`: User-scoped preference storage (category, key, value, visibility)
- `aliases`: Service usernames and handles (GitHub, Jira, Slack, Linear, Confluence)

### Connection Management

- Thread-safe connection pool via `rusqlite::Connection`
- Transactional support for ACID guarantees
- Automatic schema migrations on startup

## Usage

### Initialization

```rust
use trusty_cto_db::{Database, DatabaseConfig};

let config = DatabaseConfig {
    path: "~/.trusty/cto.db".into(),
    ..Default::default()
};
let db = Database::open(&config)?;

// Schema is automatically migrated to current version
```

### Querying

```rust
// Find a person by email
let person = db.find_person_by_email("alice@example.com")?;

// Get org chart (manager chain + directs)
let chart = db.org_chart(person_id)?;

// Find people in a pod
let pod_members = db.find_pod_members("platform")?;
```

### Mutations

```rust
// Update role
db.set_person_role(person_id, Role::Admin)?;

// Add group membership
db.add_schema_member(person_id, Group::ELT)?;

// Update preference
db.set_preference(person_id, "food.dietary", "vegetarian")?;
```

## Configuration

### Environment Variables

- `TRUSTY_CTO_DB_PATH`: SQLite database file location
  - Default: `~/.trusty/cto.db`
  - Can be overridden per-connection via `DatabaseConfig`

### Migrations

Migrations are versioned and applied automatically. To add a new migration:
1. Create a new file under `migrations/` with the next version number
2. Implement the `Migration` trait
3. Add it to the migration registry in `database.rs`

## Integration

### Used By

- **tc-services**: High-level service layer queries
- **directory MCP**: User and org lookups
- **trusty-mpm**: Person context and preferences
- **open-mpm**: Agent context and role-based access control

### Sync Sources

- **Google Workspace API**: Org unit and employee data
- **Granola**: Historical people movement and hiring signals
- **SFDC**: Opportunity and account context enrichment

## Performance

- **Indexes**: Composite indexes on frequently queried columns (email, name, pod, org_unit)
- **Connection pooling**: Reuse connections across threads
- **Query planning**: Use EXPLAIN QUERY PLAN for analysis

## Error Handling

All database operations return `Result<T, DbError>`:

```rust
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("SQL error: {0}")]
    Sql(String),
    #[error("Not found: {0}")]
    NotFound(String),
    #[error("Invalid data: {0}")]
    InvalidData(String),
}
```

## See Also

- `docs/trusty-cto-db/` for design and research
- `crates/tc-services/README.md` for service layer
- `crates/trusty-gworkspace/README.md` for sync source
