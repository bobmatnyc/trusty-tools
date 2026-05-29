# open-mpm-agent-api — MPM Agent API Types

**Purpose**: Shared type definitions and RPC interfaces for MPM agents and orchestrator communication.

**License**: Elastic License 2.0

## Design

- **Cargo cycle prevention**: Intentional separation from open-mpm to avoid circular dependencies
  - `open-mpm` imports agent-api types
  - Agent implementations import agent-api types
  - Agents do NOT import `open-mpm` (prevents full platform dependency)
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

- **open-mpm**: Orchestrator implementation (imports these types)
- **Agent implementations**: Subagents import and implement these traits
- **RPC layer**: Types are JSON-RPC compatible for stdio transport

## See Also

- `crates/open-mpm-agent-api/README.md` for full API reference
- `crates/open-mpm/README.md` for orchestrator implementation
