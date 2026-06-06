# trusty-agents-common

Shared type definitions and RPC interfaces for MPM agents and orchestrator communication.

**License**: Elastic License 2.0

## Purpose

`trusty-agents-common` defines the contract between the MPM orchestrator and agents:
- Message types for agent-orchestrator communication
- Trait definitions for agent implementation
- Context and constraint types
- Error types and result wrappers

This crate is intentionally **separate from trusty-agents** to break circular dependencies and allow agent implementations to depend on the API types without pulling in the full orchestrator.

## Cargo Cycle Prevention

**Architecture**:
```
trusty-agents-common (this crate)
  ↑
  ├── trusty-agents (orchestrator imports types)
  └── Agent implementations (import and implement traits)
```

**Why separate**:
- Agents need to know the request/response types
- Agents should NOT need to depend on the full trusty-agents orchestrator
- Separating API from implementation allows flexible agent distribution
- Reduces dependency bloat for agent crates

## Core Types

### Agent Execution

```rust
/// Request to execute a task
pub struct AgentRequest {
    pub request_id: String,
    pub goal: String,
    pub context: AgentContext,
    pub constraints: Constraints,
}

/// Response from agent execution
pub enum AgentResponse {
    Success {
        result: serde_json::Value,
        duration: Duration,
    },
    Error {
        code: String,
        message: String,
        details: Option<serde_json::Value>,
    },
    Progress {
        percent: u32,
        message: String,
    },
}
```

### Context

```rust
/// Execution environment for agent
pub struct AgentContext {
    pub session_id: String,
    pub user_id: String,
    pub working_dir: PathBuf,
    pub environment: HashMap<String, String>,
}

/// Resource and time constraints
pub struct Constraints {
    pub memory_limit_mb: u32,
    pub timeout_secs: u32,
    pub max_retries: u32,
}
```

### Agent Trait

```rust
pub trait Agent: Send + Sync {
    /// Handle incoming request
    async fn handle_request(&self, req: AgentRequest) -> AgentResponse;

    /// Cancel an in-flight request
    async fn cancel(&self, request_id: &str) -> Result<()>;

    /// Health check
    async fn health_check(&self) -> Result<()>;
}
```

## Usage

### Implementing an Agent

```rust
use trusty_agents_agent_api::{Agent, AgentRequest, AgentResponse};

struct MyAgent;

#[async_trait::async_trait]
impl Agent for MyAgent {
    async fn handle_request(&self, req: AgentRequest) -> AgentResponse {
        // Implement agent logic
        AgentResponse::Success {
            result: serde_json::json!({"status": "ok"}),
            duration: std::time::Duration::from_secs(1),
        }
    }

    async fn cancel(&self, request_id: &str) -> Result<()> {
        // Implement cancellation logic
        Ok(())
    }

    async fn health_check(&self) -> Result<()> {
        Ok(())
    }
}
```

### RPC Communication

The types are JSON-RPC 2.0 compatible for stdio transport:

```json
{
  "jsonrpc": "2.0",
  "id": "req-123",
  "method": "Agent.handle_request",
  "params": {
    "request_id": "req-123",
    "goal": "Find users named Alice",
    "context": {
      "session_id": "sess-456",
      "user_id": "user-789",
      "working_dir": "/home/user",
      "environment": {}
    },
    "constraints": {
      "memory_limit_mb": 512,
      "timeout_secs": 300,
      "max_retries": 3
    }
  }
}
```

## Error Handling

```rust
pub enum AgentError {
    #[error("Timeout: {0}")]
    Timeout(String),
    #[error("Out of memory: {0}")]
    OutOfMemory(String),
    #[error("Invalid request: {0}")]
    InvalidRequest(String),
    #[error("Agent error: {0}")]
    Internal(String),
}

pub type Result<T> = std::result::Result<T, AgentError>;
```

## Serialization

All types implement `serde::{Serialize, Deserialize}`:

```rust
use trusty_agents_agent_api::AgentRequest;

let req_json = r#"{"request_id":"123","goal":"...","context":{...}}"#;
let req: AgentRequest = serde_json::from_str(req_json)?;

let response_json = serde_json::to_string(&response)?;
```

## Integration with MPM

The orchestrator (`trusty-agents`) imports and uses these types:

```rust
// In trusty-agents
use trusty_agents_agent_api::{Agent, AgentRequest, AgentResponse};

pub struct Orchestrator {
    agents: HashMap<String, Box<dyn Agent>>,
}

impl Orchestrator {
    pub async fn dispatch(&self, agent_name: &str, req: AgentRequest) -> AgentResponse {
        let agent = self.agents.get(agent_name).expect("agent not found");
        agent.handle_request(req).await
    }
}
```

## Testing

Mock agents for testing:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    struct MockAgent {
        response: AgentResponse,
    }

    #[async_trait::async_trait]
    impl Agent for MockAgent {
        async fn handle_request(&self, _req: AgentRequest) -> AgentResponse {
            self.response.clone()
        }

        async fn cancel(&self, _id: &str) -> Result<()> {
            Ok(())
        }

        async fn health_check(&self) -> Result<()> {
            Ok(())
        }
    }
}
```

## See Also

- `crates/trusty-agents/README.md` for orchestrator implementation
- `crates/trusty-agents-local/README.md` for local execution agent
- Agent implementation examples in subdirectories
