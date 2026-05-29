# open-mpm-local

Plugin shim for MPM that executes tasks locally (Bash, file operations, process spawning). Implements the agent-api contract for the orchestrator.

**License**: Elastic License 2.0

## Purpose

`open-mpm-local` provides local process execution capabilities to the MPM orchestrator:
- Spawn shell commands (bash, zsh, sh)
- File read/write operations
- Directory traversal and file listing
- Resource limits (memory, timeout, CPU)
- Argument validation and command sandboxing

## Architecture

### Agent Implementation

Implements `open-mpm-agent-api::Agent` trait:

```rust
pub struct LocalExecutor {
    sandbox_root: PathBuf,
    max_concurrent_tasks: usize,
    timeout_secs: u32,
    memory_limit_mb: u32,
    allowed_commands: Vec<String>,
}

#[async_trait::async_trait]
impl Agent for LocalExecutor {
    async fn handle_request(&self, req: AgentRequest) -> AgentResponse {
        // Parse goal as shell command or file operation
        // Validate against allowed commands and sandbox constraints
        // Execute with resource limits
    }
}
```

### Command Execution

```rust
pub async fn execute_command(
    cmd: &str,
    args: Vec<&str>,
    cwd: &Path,
    timeout: Duration,
) -> Result<ProcessOutput> {
    // Spawn process in isolated group
    // Capture stdout/stderr
    // Enforce timeout and memory limits
    // Return output or error
}
```

### File Operations

```rust
pub async fn read_file(&self, path: &Path) -> Result<String> {
    // Validate path within sandbox_root
    // Check file permissions
    // Read and return content
}

pub async fn write_file(&self, path: &Path, content: &str) -> Result<()> {
    // Validate path within sandbox_root
    // Check write permissions
    // Write atomically
}
```

## Configuration

Via environment variables or config file:

```bash
# Maximum concurrent task executions
TRUSTY_LOCAL_MAX_CONCURRENT_TASKS=4

# Default task timeout
TRUSTY_LOCAL_TIMEOUT_SECS=300

# Memory limit per task
TRUSTY_LOCAL_MEMORY_LIMIT_MB=512

# Comma-separated allowed commands
TRUSTY_LOCAL_ALLOWED_COMMANDS=bash,zsh,sh,git,cargo

# Sandbox root (restricts file access)
TRUSTY_LOCAL_SANDBOX_ROOT=/tmp
```

Or in TOML config:

```toml
[local-executor]
max_concurrent_tasks = 4
timeout_secs = 300
memory_limit_mb = 512
allowed_commands = ["bash", "zsh", "sh", "git", "cargo"]
sandbox_root = "/tmp"
```

## Security Model

### Command Allowlist

Only whitelisted commands can be executed:
```rust
let allowed = vec!["bash", "zsh", "sh", "git", "cargo"];
if !allowed.contains(&cmd) {
    return Err(AgentError::InvalidRequest("command not allowed"));
}
```

### Sandbox Constraints

- File access restricted to `sandbox_root`
- Path traversal checks prevent escaping sandbox
- Symlinks followed but resolved within sandbox
- Sensitive env vars filtered (API keys, passwords)

### Resource Limits

- **Memory**: Process terminated if exceeds limit
- **CPU**: Process group limits via cgroups or native process APIs
- **Timeout**: Force-kill after configured duration
- **Concurrency**: Semaphore limits concurrent tasks

## Usage

### Dispatching Commands via Orchestrator

```rust
use open_mpm_agent_api::{AgentRequest, AgentContext, Constraints};
use open_mpm_local::LocalExecutor;

let executor = LocalExecutor::new(Config::default())?;

let req = AgentRequest {
    request_id: "req-123".into(),
    goal: "Run cargo test".into(),
    context: AgentContext {
        session_id: "sess-456".into(),
        user_id: "user-789".into(),
        working_dir: "/path/to/project".into(),
        environment: Default::default(),
    },
    constraints: Constraints {
        memory_limit_mb: 512,
        timeout_secs: 300,
        max_retries: 1,
    },
};

let response = executor.handle_request(req).await;
match response {
    AgentResponse::Success { result, duration } => {
        println!("Command succeeded in {:?}", duration);
        println!("Output: {}", result);
    }
    AgentResponse::Error { message, .. } => {
        eprintln!("Command failed: {}", message);
    }
    _ => {}
}
```

### Direct Command Execution

```rust
use open_mpm_local::CommandExecutor;

let executor = CommandExecutor::new(config)?;

let output = executor.execute(
    "git",
    vec!["status"],
    Duration::from_secs(30),
)?;

println!("stdout: {}", output.stdout);
println!("stderr: {}", output.stderr);
println!("exit_code: {}", output.exit_code);
```

## Error Handling

```rust
pub enum Error {
    #[error("Command not allowed: {0}")]
    CommandNotAllowed(String),

    #[error("Path outside sandbox: {0}")]
    PathOutsideSandbox(PathBuf),

    #[error("Timeout after {0}s")]
    Timeout(u32),

    #[error("Memory limit exceeded: {0}MB")]
    MemoryLimitExceeded(u32),

    #[error("Process execution failed: {0}")]
    ProcessError(String),
}
```

## Testing

```rust
#[tokio::test]
async fn test_execute_simple_command() {
    let executor = LocalExecutor::new(Config::default()).unwrap();

    let req = AgentRequest {
        request_id: "test-1".into(),
        goal: "echo hello".into(),
        // ...
    };

    let response = executor.handle_request(req).await;
    assert!(matches!(response, AgentResponse::Success { .. }));
}

#[tokio::test]
async fn test_command_not_allowed() {
    let mut config = Config::default();
    config.allowed_commands = vec!["echo".to_string()];
    let executor = LocalExecutor::new(config).unwrap();

    let req = AgentRequest {
        request_id: "test-2".into(),
        goal: "rm -rf /".into(),  // blocked
        // ...
    };

    let response = executor.handle_request(req).await;
    assert!(matches!(response, AgentResponse::Error { .. }));
}
```

## Integration

### With open-mpm Orchestrator

The orchestrator registers this agent:

```rust
let local = LocalExecutor::new(config)?;
orchestrator.register_agent("local", Box::new(local))?;

// Then dispatch work:
let response = orchestrator.dispatch("local", request).await;
```

### In Multi-Agent Systems

Used alongside other agents (code-intelligence, git, deploy, etc.):

```
Orchestrator
├── local (this crate)      -- command execution, file ops
├── git-agent               -- git operations
├── deploy-agent            -- deployment orchestration
└── code-analysis-agent     -- code analysis
```

## Performance

- **Startup**: <100ms for executor init
- **Command overhead**: ~50ms per spawn
- **Resource tracking**: Minimal overhead via process syscalls
- **Concurrent execution**: Limited by `max_concurrent_tasks` (default 4)

## See Also

- `crates/open-mpm-agent-api/README.md` for agent trait definition
- `crates/open-mpm/README.md` for orchestrator
- Security best practices in `docs/open-mpm-local/`
