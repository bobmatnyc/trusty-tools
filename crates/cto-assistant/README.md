# cto-assistant

CTO assistant CLI application for querying organizational context, managing employee directory, and administering roles and permissions.

**License**: Elastic License 2.0

## Purpose

`cto-assistant` provides command-line tools for:
- Querying employee directory (find people, org charts, contact info)
- Managing roles and access control (admin, superadmin, readonly)
- Syncing data from Google Workspace and other sources
- Searching and managing user preferences
- Auditing organizational changes

## Binary

The crate builds a CLI binary: `cto-assistant` (or `cto` via alias).

```bash
cto-assistant --help
cto-assistant people find --name Alice
cto-assistant roles list
cto-assistant preferences set alice@example.com food.dietary vegetarian
```

## Command Groups

### people — Employee Directory

```bash
# Find people
cto people find --name Alice --department Engineering
cto people find --email alice@example.com
cto people list --pod platform-team
cto people list --status active

# Show org chart
cto people org-chart --email alice@example.com

# Show person details
cto people show alice@example.com
```

### roles — Access Control

```bash
# List roles
cto roles list
cto roles list --person alice@example.com

# Assign role
cto roles set alice@example.com admin
cto roles set bob@example.com readonly

# Remove role (revert to readonly)
cto roles unset alice@example.com
```

### groups — Group Membership

```bash
# List group members
cto groups list --name ELT
cto groups list --name SELT

# Add member
cto groups add alice@example.com ELT

# Remove member
cto groups remove alice@example.com ELT
```

### preferences — User Preferences

```bash
# Get preference
cto preferences get alice@example.com food.dietary

# Set preference
cto preferences set alice@example.com food.dietary vegetarian
cto preferences set alice@example.com identity.nickname Ali

# List all preferences
cto preferences list alice@example.com

# Delete preference
cto preferences delete alice@example.com food.dietary
```

### sync — Data Synchronization

```bash
# Sync from Google Workspace
cto sync google-workspace

# Sync from Granola
cto sync granola

# Show sync status
cto sync status

# View recent syncs
cto sync history --limit 10
```

## Configuration

### CLI Flags

```bash
# Specify database path
cto --db-path ~/.trusty/cto.db people find --name Alice

# Verbose output
cto --verbose roles list

# JSON output
cto --json people find --name Alice

# No colors
cto --no-color org-chart
```

### Environment Variables

- `TRUSTY_CTO_DB_PATH`: Path to CTO database (default: `~/.trusty/cto.db`)
- `TRUSTY_CTO_ADMIN`: Restrict to admin users (when set to email)
- `RUST_LOG`: Tracing filter (default: info)

### Config File

Create `~/.trusty/cto.toml`:

```toml
[database]
path = "~/.trusty/cto.db"

[sync]
auto_sync_on_startup = true
sync_interval_secs = 3600

[output]
color = true
json_output = false
```

## Architecture

### Dependencies

- **tc-services**: Service layer queries
- **trusty-common**: Tracing, error handling
- **clap**: CLI argument parsing
- **serde**: JSON serialization
- **tokio**: Async runtime

### Command Execution

```
Input (CLI args)
  ↓
Parse with clap
  ↓
Resolve to service call (tc-services)
  ↓
Execute query/mutation
  ↓
Format output (table, JSON, etc.)
  ↓
Display to stdout
```

## Output Formats

### Table (default)

```
NAME     EMAIL                DEPARTMENT  POD    MANAGER
Alice    alice@example.com    Engineering platform Bob
Bob      bob@example.com      Engineering platform Carol
Carol    carol@example.com    Leadership  na     CEO
```

### JSON (--json flag)

```json
{
  "people": [
    {
      "name": "Alice",
      "email": "alice@example.com",
      "department": "Engineering",
      "pod": "platform",
      "manager_email": "bob@example.com"
    }
  ]
}
```

## Security & Permissions

- **Admin-only commands**: Role management, preference deletion, sync operations
- **Self-service access**: Users can read/write their own preferences
- **Audit logging**: All mutations logged to stderr
- **No credentials stored**: OAuth tokens in secure vault, not filesystem

## Exit Codes

- `0`: Success
- `1`: General error (usage, invalid input)
- `2`: Permission denied (not admin)
- `3`: Not found
- `4`: Database error
- `5`: API error (Google Workspace, etc.)

## Examples

### Find a person and show org chart

```bash
$ cto people find --email alice@example.com
NAME  EMAIL                TITLE              DEPARTMENT
Alice alice@example.com    Senior Engineer    Engineering

$ cto people org-chart alice@example.com
├── Alice (alice@example.com)
│   └── Manager: Bob (bob@example.com)
│       └── Manager: Carol (carol@example.com)
│           └── Manager: CEO (ceo@example.com)
```

### Manage roles

```bash
$ cto roles list --person alice@example.com
alice@example.com: readonly

$ cto roles set alice@example.com admin
Updated alice@example.com to admin

$ cto roles list --person alice@example.com
alice@example.com: admin
```

### Sync and verify

```bash
$ cto sync google-workspace
Syncing from Google Workspace...
Updated 153 people
Updated 12 org units
Sync completed in 2.3s

$ cto sync status
Last sync: 5 minutes ago (Google Workspace)
Status: Success
```

## Testing

```bash
# Run unit tests
cargo test -p cto-assistant

# Integration tests (requires running database)
cargo test -p cto-assistant -- --include-ignored

# Test a specific command
cargo test -p cto-assistant test_people_find
```

## Error Handling

All commands provide clear error messages:

```bash
$ cto people find --name NonExistent
Error: No people found matching "NonExistent"
Try 'cto people find --help' for usage

$ cto roles set alice@example.com badpermission
Error: Invalid role "badpermission". Valid roles: admin, readonly, superadmin
```

## Integration

### With trusty-mpm

MPM agents can query directory context:

```rust
// Inside agent code
let person = directory_service.find_by_email("alice@example.com")?;
let org_chart = directory_service.org_chart(&person.id)?;
```

### With trusty-agents

Orchestrator can dispatch directory lookup tasks:

```bash
# CLI invocation
cto people find --pod platform

# Programmatic invocation
orchestrator.dispatch("cto-assistant", request).await
```

## See Also

- `crates/tc-services/README.md` for service layer
- `crates/trusty-cto-db/README.md` for database schema
- `docs/cto-assistant/` for design and research
