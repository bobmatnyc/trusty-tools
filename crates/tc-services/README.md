# tc-services

High-level service layer adapters for CTO database, Granola API, and Google Workspace clients. Provides unified query and mutation interfaces for organizational context, user preferences, and directory information.

**License**: Elastic License 2.0

## Purpose

`tc-services` wraps raw database and API access with:
- Convenient query APIs (find people, org charts, roles, preferences)
- Caching for frequently accessed data
- Error handling and retry logic
- Transactional mutations for consistency
- Integration with third-party sync sources (Granola, Google Workspace)

## Architecture

### Core Services

#### DirectoryService
Query and update organizational directory:
```rust
pub trait DirectoryService {
    async fn find_person_by_email(&self, email: &str) -> Result<Person>;
    async fn find_people_in_pod(&self, pod: &str) -> Result<Vec<Person>>;
    async fn org_chart(&self, person_id: &str) -> Result<OrgChartNode>;
}
```

#### RoleService
Query and update access control:
```rust
pub trait RoleService {
    async fn get_person_role(&self, person_id: &str) -> Result<Role>;
    async fn set_person_role(&self, person_id: &str, role: Role) -> Result<()>;
    async fn find_schema_members(&self, schema: Schema) -> Result<Vec<Person>>;
}
```

#### PreferenceService
Store and retrieve user preferences:
```rust
pub trait PreferenceService {
    async fn get_preference(&self, person_id: &str, key: &str) -> Result<Option<String>>;
    async fn set_preference(&self, person_id: &str, key: &str, value: &str) -> Result<()>;
    async fn list_preferences(&self, person_id: &str) -> Result<Vec<Preference>>;
}
```

#### SyncService
Coordinate data synchronization:
```rust
pub trait SyncService {
    async fn sync_from_google_workspace(&self) -> Result<SyncStats>;
    async fn sync_from_granola(&self) -> Result<SyncStats>;
    async fn sync_status(&self) -> Result<LastSyncInfo>;
}
```

### Caching Strategy

- **LRU in-memory cache**: Frequently accessed people/orgs
- **Cache invalidation**: On mutations, via event broadcasts
- **TTL**: Configurable per service (default: 1 hour)
- **Bypass option**: Force-refresh flag on queries

## Configuration

```rust
pub struct Config {
    pub db_path: PathBuf,
    pub cache_capacity: usize,     // entries
    pub cache_ttl: Duration,        // per service
    pub google_workspace_secret: String,
    pub granola_api_key: String,
}

let services = Services::new(config)?;
```

### Environment Variables

- `TRUSTY_CTO_DB_PATH`: Path to CTO database
- `TRUSTY_CACHE_CAPACITY`: Max entries in LRU cache (default: 10000)
- `TRUSTY_CACHE_TTL_SECS`: Cache TTL in seconds (default: 3600)
- `GOOGLE_WORKSPACE_SECRET`: OAuth credentials for GWorkspace sync
- `GRANOLA_API_KEY`: API key for Granola historical data

## Usage

### Query Directory

```rust
let person = services.directory()
    .find_person_by_email("alice@example.com")
    .await?;

let org_chart = services.directory()
    .org_chart(&person.id)
    .await?;

let pod_members = services.directory()
    .find_people_in_pod("platform")
    .await?;
```

### Update Roles

```rust
services.roles()
    .set_person_role(&person_id, Role::Admin)
    .await?;

services.roles()
    .add_schema_member(&person_id, Schema::ELT)
    .await?;
```

### Store Preferences

```rust
services.preferences()
    .set_preference(&person_id, "food.dietary", "vegetarian")
    .await?;

let prefs = services.preferences()
    .list_preferences(&person_id)
    .await?;
```

### Sync Data

```rust
let stats = services.sync()
    .sync_from_google_workspace()
    .await?;

println!("Synced {} people from Google Workspace", stats.people_updated);
```

## Integration Points

### Consumers

- **directory MCP**: User and org lookups via MCP API
- **trusty-mpm**: Context and preferences for agent dispatch
- **open-mpm**: Directory context for agent workflows
- **tc-assistant**: CTO assistant CLI commands

### Data Sources

- **trusty-cto-db**: Local SQLite persistence
- **Google Workspace API**: Org units, people, managers
- **Granola**: Historical org changes and hiring signals

## Error Handling

All service methods return `Result<T, ServiceError>`:

```rust
#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error("Database error: {0}")]
    Database(#[from] DbError),
    #[error("API error: {0}")]
    Api(String),
    #[error("Not found: {0}")]
    NotFound(String),
    #[error("Permission denied: {0}")]
    PermissionDenied(String),
}
```

## Testing

Use `MockDirectoryService` and other mocks for unit tests:

```rust
#[tokio::test]
async fn test_org_chart() {
    let mut mock = MockDirectoryService::new();
    mock.expect_org_chart()
        .returning(|_| Ok(/* ... */));
    
    let result = mock.org_chart("person-123").await;
    assert!(result.is_ok());
}
```

## Performance

- **Connection pooling**: Reuse DB connections across service calls
- **Caching**: Avoid repeated queries for same data
- **Batch operations**: Use `find_people_in_pod`, `find_schema_members` for bulk queries
- **Indexed queries**: Leverage DB indexes on email, name, pod, org_unit

## See Also

- `crates/trusty-cto-db/README.md` for database layer
- `crates/trusty-gworkspace/README.md` for Google Workspace integration
- `docs/tc-services/` for architecture and design
