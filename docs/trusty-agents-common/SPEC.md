# trusty-agents-common — Agent API Types

**Purpose**: Shared type definitions and RPC interfaces for trusty-agents sub-agents and orchestrator communication.

**License**: MIT

## Design

- **Cargo cycle prevention**: Intentional separation from trusty-agents to avoid circular dependencies
  - `trusty-agents` imports agent-common types
  - Agent implementations import agent-common types
  - Agents do NOT import `trusty-agents` (prevents full platform dependency)
- **Serialization**: serde/JSON-RPC 2.0 compatible types
- **Async traits**: Async-trait for agent handlers

## API Surfaces

### Agent Message Types
- `AgentRequest`: Inbound message to agent (goal, context, constraints)
- `AgentResponse`: Outbound message from agent (result, error, streaming updates)
- `AgentHeartbeat`: Health/status signals from running agent

### Context Types
- `AgentContext`: Execution environment (session ID, request ID, user info)
- `Memory`: Persistent and ephemeral memory state
- `Constraints`: Resource limits, time bounds, retry budgets

### Handler Traits
```rust
pub trait Agent {
    async fn handle_request(&self, req: AgentRequest) -> AgentResponse;
    async fn cancel(&self, request_id: &str) -> Result<()>;
}
```

## Integration Points

- **trusty-agents**: Orchestrator implementation (imports these types)
- **Agent implementations**: Subagents import and implement these traits
- **RPC layer**: Types are JSON-RPC compatible for stdio transport

## See Also

- [`crates/trusty-agents-common/README.md`](../../crates/trusty-agents-common/README.md) for full API reference
- [`crates/trusty-agents/README.md`](../../crates/trusty-agents/README.md) for orchestrator implementation
