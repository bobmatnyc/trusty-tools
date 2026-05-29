# open-mpm-local — MPM Local Execution Plugin

**Purpose**: Plugin shim for MPM that executes tasks locally (Bash, file operations, process spawning).

**License**: Elastic License 2.0

## Design

- **Local-only execution**: No remote dispatch, all work runs on local machine
- **Plugin interface**: Implements MPM agent-api contract for orchestrator compatibility
- **Resource isolation**: Process namespaces and file permission checks
- **Command sandboxing**: Restricted command whitelist and argument validation

## Capabilities

### Process Execution
- Spawn shell commands (bash, zsh, sh)
- Working directory control
- Environment variable passing
- Timeout enforcement
- Capture stdout/stderr

### File Operations
- Read/write files (with permission checks)
- Directory traversal (safe, sandboxed)
- File listing and metadata
- Temporary file cleanup

### Resource Control
- Memory limits per task
- CPU limits via process groups
- Timeout enforcement
- Concurrent task limits

## Configuration

```toml
[local-executor]
max_concurrent_tasks = 4
timeout_secs = 300
memory_limit_mb = 512
allowed_commands = ["bash", "zsh", "sh", "git", "cargo"]
sandbox_root = "/tmp"
```

## Integration Points

- **open-mpm orchestrator**: Agent-api implementation
- **MPM agents**: Dispatch local tasks via orchestrator
- **File system**: Access restricted to configured sandbox root

## Security Notes

- Command allowlist prevents arbitrary execution
- File access restricted to designated paths
- Environment variable filtering removes secrets
- Process isolation via separate process groups

## See Also

- `crates/open-mpm-local/README.md` for full API
- `crates/open-mpm/README.md` for orchestrator context
