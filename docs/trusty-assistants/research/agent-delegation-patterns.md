# Agent Delegation Patterns — Research Report

**Date:** 2026-04-22  
**Scope:** How multi-agent systems handle PM→sub-agent delegation, result passing, tool routing, and parallel execution; applied to open-mpm design

---

## 1. Foundational Patterns

### 1a. Orchestrator-Worker (Dominant Pattern)

Accounts for ~70% of production multi-agent deployments. The orchestrator:
1. Receives a task
2. Classifies intent and decomposes into subtasks
3. Routes subtasks to specialized workers
4. Executes workers in parallel where possible
5. Aggregates results

Offers predictable control flow, centralized observability, and clean separation of concerns. This is the pattern claude-mpm uses: PM orchestrator → specialized sub-agents.

### 1b. Agents as Tools

An agent is wrapped as a callable tool in the orchestrator's tool list. The orchestrator LLM decides when to invoke it, just like any other function call.

**Best for:** When a specialist should help with a bounded subtask but should not take over the user-facing conversation.

```
Orchestrator LLM
  tools: [bash, read_file, engineer_agent, qa_agent]
  
  LLM produces: tool_call { name: "engineer_agent", args: { task: "implement auth" } }
  → Orchestrator invokes engineer sub-agent
  → Returns result as tool result message
  → LLM continues with result
```

This is what Rig demonstrates in `agent_with_agent_tool.rs` and what the OpenAI Agents SDK supports via `Agent.as_tool()`.

### 1c. Handoffs

The orchestrator passes control entirely to a specialized agent. The specialist becomes the active agent for the remainder of the turn.

**Best for:** Open-ended tasks where you want the specialist to fully own execution.

```
Triage Agent
  → classifies: "this is a coding task"
  → handoff: { target: "engineer_agent", context: {...} }
  
Engineer Agent
  ← receives full context and responsibility
  → executes autonomously
  → returns final result to user
```

Key distinction from agents-as-tools: handoffs pass responsibility; tools-as-agents preserve orchestrator control.

### 1d. Sequential Pipeline

Steps build on each other. Output of step N becomes input of step N+1.

```
PM → Research Agent → Summarizer Agent → Writer Agent → Output
```

Good for: document processing, multi-stage refinement, code review pipelines.

### 1e. Parallel Fan-out / Fan-in

Multiple agents work simultaneously on the same task from different angles. Results are aggregated.

```
         ┌── Agent A (approach 1) ──┐
PM Task ──┤── Agent B (approach 2) ──┤── Aggregator → Final Output
         └── Agent C (approach 3) ──┘
```

Good for: research (explore multiple sources), code review (security + quality + style), hypothesis generation.

---

## 2. PM Orchestrator Design (Anthropic Research System)

Anthropic's internal multi-agent research system provides a reference implementation:

### Lead Agent Behavior

1. User submits query
2. Lead agent analyzes it and develops a strategy
3. Lead agent decomposes query into subtasks
4. Spawns subagents to explore different aspects **simultaneously**
5. Subagents run in parallel
6. Lead agent aggregates findings

### Task Description Requirements

Each sub-agent delegation MUST include:
- **Objective:** What needs to be accomplished
- **Output format:** How results should be structured
- **Tool/source guidance:** Which tools and data sources to use
- **Task boundaries:** What is in/out of scope

Without detailed task descriptions, agents duplicate work, leave gaps, or fail to find necessary information.

### Effort Scaling

Agents struggle to judge appropriate effort for different tasks. Embed scaling rules in prompts explicitly:
- Simple lookup → 1-2 tool calls
- Research task → up to N iterations
- Complex implementation → time-boxed execution

---

## 3. OpenAI Agents SDK Patterns (Reference Implementation)

The OpenAI Agents SDK (Python) provides the most complete reference for PM orchestration patterns. Key architectural choices:

### Agents as Tools Pattern

```python
# Python reference — translate to Rust patterns
engineer_agent = Agent(
    name="Engineer",
    instructions="You implement code based on specifications.",
    tools=[bash_tool, read_file_tool, write_file_tool]
)

pm_agent = Agent(
    name="PM",
    instructions="You orchestrate software development tasks.",
    tools=[
        engineer_agent.as_tool(
            tool_name="engineer",
            tool_description="Delegate coding tasks to the engineer specialist"
        )
    ]
)
```

### Parallel Execution

Use `asyncio.gather` (Python) / `tokio::join!` or `futures::future::join_all` (Rust) for parallel sub-agents:

```rust
// Rust equivalent
let (research_result, analysis_result) = tokio::join!(
    run_sub_agent("researcher", research_task),
    run_sub_agent("analyst", analysis_task)
);
```

### Result Passing via Structured Outputs

Results are typed, not free-form text. Use structured output schemas to ensure PM can reliably parse sub-agent results:

```rust
#[derive(Serialize, Deserialize)]
struct AgentResult {
    status: TaskStatus,
    output: String,
    artifacts: Vec<Artifact>,
    next_steps: Option<Vec<String>>,
}
```

---

## 4. Tool Call Routing

In a multi-agent system, the PM must route tool calls correctly. Three routing strategies:

### 4a. PM-Owned Tools

The PM owns certain tools (project planning, file management) and handles them directly.

### 4b. Delegated Tool Execution

Sub-agents own domain-specific tools. PM delegates the task; sub-agent handles its own tool calls internally.

```
PM delegates "implement auth" to Engineer Agent
    Engineer Agent: calls bash → runs tests
    Engineer Agent: calls write_file → creates auth.rs
    Engineer Agent: returns completed result to PM
```

### 4c. Tool Proxying

PM intercepts all tool calls, applies policy (rate limits, logging, permissions), and forwards to execution. claude-mpm uses a hook/event system for this.

```
Sub-Agent → tool_call request → PM intercepts → validates/logs → executes → returns result
```

---

## 5. Message Protocol Design

### JSON-RPC 2.0 for Agent Communication

JSON-RPC 2.0 is the natural choice for agent communication (it's what MCP uses):

```json
// PM → Sub-agent: delegate a task
{
  "jsonrpc": "2.0",
  "id": "task-001",
  "method": "execute_task",
  "params": {
    "task_type": "implementation",
    "description": "Implement JWT authentication",
    "context": {
      "codebase_summary": "...",
      "relevant_files": ["src/auth.rs", "src/main.rs"]
    },
    "constraints": {
      "timeout_secs": 300,
      "max_tool_calls": 50,
      "allowed_tools": ["bash", "read_file", "write_file", "edit_file"]
    }
  }
}

// Sub-agent → PM: progress update
{
  "jsonrpc": "2.0",
  "method": "progress",
  "params": {
    "task_id": "task-001",
    "phase": "implementing",
    "message": "Writing JWT token validation...",
    "tool_calls_used": 7
  }
}

// Sub-agent → PM: completion
{
  "jsonrpc": "2.0",
  "id": "task-001",
  "result": {
    "status": "completed",
    "summary": "Implemented JWT auth with RS256 signing",
    "files_modified": ["src/auth.rs", "src/auth/jwt.rs"],
    "tests_added": 3,
    "tests_passing": true
  }
}

// Sub-agent → PM: requesting a tool (if tool-proxying model)
{
  "jsonrpc": "2.0",
  "id": "tool-call-042",
  "method": "tool_call",
  "params": {
    "tool": "bash",
    "args": { "command": "cargo test --lib auth" }
  }
}
```

---

## 6. Agent Definition Files

### claude-mpm Approach (Python reference)

claude-mpm uses YAML-based agent definitions deployed from Git. Each agent has:
- Type identifier
- System prompt / instructions
- Allowed tools
- Domain capabilities

### ai-agents.rs Approach (Rust reference)

YAML-first agent definitions:
```yaml
# engineer_agent.yaml
name: engineer
model: anthropic/claude-sonnet-4.6
preamble: |
  You are a senior Rust engineer. You implement code based on specifications.
  Follow existing patterns in the codebase.
tools:
  - bash
  - read_file
  - write_file
  - edit_file
memory:
  type: sliding_window
  max_messages: 20
```

### Recommended for open-mpm

Agent definitions as TOML files (natural for Rust projects) or YAML:

```toml
# agents/engineer.toml
[agent]
id = "engineer"
name = "Engineer Agent"
description = "Implements code based on specifications"

[agent.model]
provider = "openrouter"
model_id = "anthropic/claude-sonnet-4.6"

[agent.system_prompt]
text = """
You are a senior software engineer working within the open-mpm framework.
Your task is to implement code based on the specification provided.
"""

[agent.tools]
allowed = ["bash", "read_file", "write_file", "edit_file", "grep", "glob"]

[agent.limits]
max_turns = 50
timeout_secs = 300
```

---

## 7. Skill / Context Loading

### claude-mpm Three-Tier Priority System

1. **Bundled skills** — shipped with the framework
2. **User skills** — user's `~/.claude/skills/` directory
3. **Project skills** — `.claude/skills/` in the project root

Skills are loaded on-demand (progressive disclosure) to optimize context window usage.

### Recommended for open-mpm

Same three-tier system in Rust:

```
$HOME/.open-mpm/skills/  # user-level skills
<project>/.open-mpm/skills/  # project-level skills
<binary>/skills/  # embedded/bundled skills
```

Skills are markdown files injected into the system prompt when relevant. The PM decides which skills to load based on task type.

---

## 8. Resilience and Fallback

### Retry with Fallback Hierarchy

```
Primary specialist agent
  ↓ (if fails after N retries)
Alternative specialist agent
  ↓ (if fails)
Simpler rule-based approach
  ↓ (if fails)
Human escalation
```

### Circuit Breaker

If a sub-agent type repeatedly fails, stop delegating to it and use an alternative strategy. claude-mpm explicitly implements a circuit breaker enforcement pattern.

### Context Window Management

Sub-agents should summarize their work before context is exhausted. claude-mpm auto-pauses at 70%/85%/95% thresholds with automatic 10k-token summaries.

---

## 9. Multi-Agent Communication Protocols (2025 Standards)

Two emerging standards:

| Protocol | Purpose | Status |
|----------|---------|--------|
| **MCP (Model Context Protocol)** | Agent ↔ tool/resource communication | March 2025 spec update, widely adopted |
| **A2A (Agent-to-Agent)** | Peer agent coordination, delegation, negotiation | Emerging, Google-backed |

For open-mpm, MCP stdio transport is the natural choice for PM ↔ sub-agent communication:
- Each sub-agent runs as an MCP server
- PM runs as an MCP client
- Tool calls routed through MCP protocol
- `rmcp` crate (v1.5.0) provides ready implementation

---

## 10. Observability

Production multi-agent systems require:

- **Trace IDs** — propagated across all agent invocations
- **Span per sub-agent** — start time, end time, tool calls made
- **Token accounting** — input/output per agent call
- **Tool call log** — every tool invoked, args, result, latency
- **OpenTelemetry** — standard export format

AutoAgents integrates `autoagents-telemetry` (OpenTelemetry). For open-mpm, `tracing` + `opentelemetry-otlp` crates are the Rust standard.

---

## Sources

- [OpenAI Agents SDK — Multi-agent orchestration](https://openai.github.io/openai-agents-python/multi_agent/)
- [Anthropic — How we built our multi-agent research system](https://www.anthropic.com/engineering/multi-agent-research-system)
- [Azure Architecture — AI Agent Orchestration Patterns](https://learn.microsoft.com/en-us/azure/architecture/ai-ml/guide/ai-agent-design-patterns)
- [Google ADK — Multi-agent systems](https://google.github.io/adk-docs/agents/multi-agents/)
- [Google Developers — Multi-agent patterns in ADK](https://developers.googleblog.com/developers-guide-to-multi-agent-patterns-in-adk/)
- [Small Model as Master Orchestrator (arxiv)](https://arxiv.org/html/2604.17009)
- [The Orchestration of Multi-Agent Systems (arxiv)](https://arxiv.org/html/2601.13671v1)
- [claude-mpm GitHub](https://github.com/bobmatnyc/claude-mpm)
- [ai-agents.rs YAML philosophy](https://ai-agents.rs/blog/why-yaml/)
