# Rust AI Agent Frameworks — Research Report

**Date:** 2026-04-22  
**Scope:** Existing Rust AI agent frameworks relevant to open-mpm design

---

## 1. Rig (0xPlaygrounds)

**Repository:** https://github.com/0xPlaygrounds/rig  
**Docs:** https://rig.rs / https://docs.rs/rig-core  
**Maturity:** Active, production-used, breaking changes possible

### Architecture

Rig raises the abstraction level for LLM applications the same way Actix/Axum did for web services. Everything is async/await via tokio and uses the builder pattern. Core abstractions:

- **Agent** — an LLM + preamble (system prompt) + tools + optional RAG context
- **Tool** — a callable function with a JSON schema descriptor
- **Completion/Embedding** — provider-agnostic traits over 20+ model providers
- **Vector Store** — 10+ backends (Qdrant, MongoDB, etc.) for RAG

### Tool Calling Support

Full tool/function calling support. Tools implement a `Tool` trait. The agent automatically handles the tool-call loop (detect `finish_reason: tool_calls`, execute locally, return result, re-invoke). An official example (`agent_with_agent_tool.rs`) shows one agent wrapping another agent as a tool — the outer agent routes to the inner agent as needed. A GitHub issue (#408) explicitly tracks "agent/vector_store as tool" as a first-class feature.

### Subprocess Support

No native subprocess or process management utilities. Rig is an LLM application framework, not a process orchestrator. You would layer `tokio::process` on top.

### Multi-Agent Delegation

Supported via the "agent as tool" pattern. An orchestrator agent declares sub-agents as tools in its tool list. When the LLM requests a sub-agent, the orchestrator's tool dispatcher invokes the sub-agent and returns results as a tool result message. There is also a state-machine example repo (`rig-agent-state-machine-example`) demonstrating structured agent orchestration with deterministic state transitions.

### Strengths for open-mpm

- Unified interface across 20+ providers (including OpenRouter)
- First-class async streaming completions
- Clean trait-based extensibility
- Active development (GitHub momentum high)
- Agent-as-tool already demonstrated

### Gaps for open-mpm

- No subprocess/process management
- No built-in skill/agent definition file format
- No PM/sub-agent orchestration protocol
- Agent delegation is manual (no built-in orchestrator loop)

---

## 2. Swiftide

**Repository:** https://github.com/bosun-ai/swiftide  
**Docs:** https://swiftide.rs / https://docs.rs/swiftide  
**Maturity:** Active but under heavy development; breaking changes frequent

### Architecture

Swiftide is built around streaming, async, parallel pipelines. Three major subsystems:

- **Indexing Pipeline** — Loader → Transformer → Chunker → Embedder → Storage (for RAG)
- **Query Pipeline** — retrieval + prompt injection (experimental)
- **Agent Framework** — tool-using agents with modular building blocks (experimental)

The `swiftide-agents` crate provides the agent layer. Pipelines and agents can be mixed.

### Tool Calling Support

Supported in the agent framework. Agents can be given tools; the framework handles the call-response loop. Building blocks are composable: "build agents, mix and match with previously built pipelines."

### Subprocess Support

None native. Pipeline steps could wrap subprocess calls, but there is no first-class process management.

### Multi-Agent Support

Agents can call other agents via the tool mechanism. The docs mention "call other agents" as a capability but the agent framework is labeled experimental.

### Strengths for open-mpm

- Excellent for RAG pipelines if open-mpm needs document/code retrieval
- Very clean pipeline composition model
- Good integrations: OpenAI, Groq, Qdrant, LanceDB, Redis, Ollama, FastEmbed, Fluvio, Treesitter
- RAGAS evaluation support

### Gaps for open-mpm

- Agent framework still experimental
- No subprocess/process orchestration
- Focus is RAG-first, not orchestration-first
- Breaking changes risk

---

## 3. llm-chain

**Repository:** https://github.com/sobelio/llm-chain  
**Docs:** https://llm-chain.xyz / https://docs.rs/llm-chain  
**Maturity:** Somewhat stale (last notable updates ~2023-2024); macro-heavy

### Architecture

Chain-based execution model:

- **Step** — a single LLM invocation (prompt + config)
- **Chain** — composition of steps: Sequential, MapReduce, or Conversational
- **Frame** — combines a Step with an Executor
- **Parameters** — data passed between steps
- **Executor** — the LLM backend (OpenAI, LLaMA, llm.rs)

### Tool Calling Support

Tools supported (bash commands, Python scripts, web search). However, the tool integration is not as ergonomic as Rig or modern function-calling APIs. Relies on pre-function-calling era patterns in places.

### Subprocess Support

Indirect. Bash tool integration implies subprocess execution. No structured process management.

### Multi-Agent Support

Not a first-class feature. Sequential/MapReduce chains can approximate pipelines but there is no orchestrator-worker delegation pattern.

### Strengths for open-mpm

- Well-developed prompt template system
- MapReduce chain is useful for large document processing
- Conversational chains with memory

### Gaps for open-mpm

- Appears less actively maintained than Rig/Swiftide
- No multi-agent delegation
- Tool calling less ergonomic than modern OpenAI function calling

---

## 4. langchain-rust (Abraxas-365)

**Repository:** https://github.com/Abraxas-365/langchain-rust  
**Maturity:** Community-maintained port; not official LangChain

### Architecture

Port of Python LangChain concepts to Rust:

- **AgentExecutor** — the agent loop runner
- **OpenAiToolAgentBuilder** — builds tool-calling agents (OpenAI function calling style)
- **Tool trait** — implement `name()`, `description()`, `run()` for custom tools
- **SimpleMemory** — in-memory conversation history
- Built-in tools: `DuckDuckGoSearchResults`, `SerpApi`, `CommandExecutor`
- **ChainCallOptions** — configuration for chain/agent invocation

### Tool Calling Support

Full OpenAI-style tool/function calling via `OpenAiToolAgentBuilder`. Custom tools via the `Tool` trait. ReAct-style agent loop. Example at `examples/open_ai_tools_agent.rs`.

### Subprocess Support

`CommandExecutor` tool runs shell commands as a tool available to the LLM. Not a process orchestrator.

### Multi-Agent Support

Not explicit. Would need to implement agent-as-tool manually.

### Strengths for open-mpm

- Familiar LangChain patterns for developers coming from Python
- OpenAI-style tool calling works with OpenRouter

### Gaps for open-mpm

- Not officially maintained
- Limited compared to native Rig patterns
- No orchestration layer

---

## 5. AutoAgents (Liquidos AI)

**Repository:** https://github.com/liquidos-ai/AutoAgents  
**Docs:** https://liquidos-ai.github.io/AutoAgents/  
**Released:** December 24, 2025  
**Maturity:** New but actively backed by LiquidOS platform

### Architecture

Event-driven, actor-model architecture using Ractor (Rust actor framework):

- **Environment** — manages one or more agents
- **Agent** — autonomous entity with Tools + Memory + Executor
- **Executor** — ReAct or basic loop
- **Protocol/Event types** — typed pub/sub communication
- **LLM Providers** — OpenAI, Anthropic, OpenRouter, Groq, Google, Azure, xAI, DeepSeek, Ollama
- **Crate structure:**
  - `autoagents-core` — core agent framework
  - `autoagents-protocol` — shared protocol/event types
  - `autoagents-llm` — LLM provider implementations
  - `autoagents-telemetry` — OpenTelemetry
  - `autoagents-toolkit` — ready-to-use tools
  - `autoagents-guardrails` — LLM output safety
  - `autoagents-derive` — procedural macros for tools
  - `autoagents-mistral-rs` / `autoagents-llamacpp` — local inference backends

### Tool Calling Support

Tool definitions via derive macros (`#[tool]` attribute). Structs deriving `JsonSchema` define parameters. Supports both cloud and local model tool calling.

### Subprocess Support

WASM sandboxed runtime for tool execution. OS-level process isolation for agents in the LiquidOS platform. Not the same as general subprocess IPC but shows security-conscious process isolation thinking.

### Multi-Agent Support

First-class multi-agent via Ractor actor model. Typed pub/sub communication between agents. Environment manages lifecycle. Parallel agent execution via async.

### Strengths for open-mpm

- Native OpenRouter support
- Actor model maps well to agent delegation
- Strong security focus (WASM sandboxing, process isolation)
- OpenTelemetry for observability
- ReAct executor built-in

### Gaps for open-mpm

- Very new (December 2025) — API stability unknown
- No built-in skill/agent definition file format
- No claude-mpm-style PM→sub-agent subprocess delegation

---

## 6. Official Rust MCP SDK (rmcp)

**Repository:** https://github.com/modelcontextprotocol/rust-sdk  
**Crate:** `rmcp` v1.5.0 (April 16, 2026)  
**Maturity:** Official Anthropic/MCP; 3.3k stars, 74 releases, very mature

### Architecture

Implements the full MCP specification:

- **Transports:** stdio (local subprocess), SSE (HTTP streaming), Streamable HTTP
- **Tool macros:** `#[tool]` attribute + `JsonSchema` derive = auto-generated tool schema
- **ClientHandler / ServerHandler** traits for both sides
- **TokioChildProcess** — spawn an MCP server as a subprocess and connect a client

```rust
let client = ().serve(TokioChildProcess::new(
    Command::new("npx").configure(|cmd| {
        cmd.arg("-y").arg("@modelcontextprotocol/server-everything");
    })
)?).await?;
```

### Significance for open-mpm

The official MCP SDK gives open-mpm a proven IPC mechanism. Sub-agents can be spawned as MCP servers (stdio transport), and the PM connects as an MCP client. Tool calling, resource access, and message passing are all handled by the protocol. This is the most battle-tested Rust subprocess communication library in the AI agent space.

---

## 7. Other Notable Frameworks

### Kalosm
- Candle-based interface for multimodal local models
- Focus: local inference, not cloud orchestration
- No tool calling or agent delegation

### Nerve
- YAML-based multi-step agent definition
- Declarative agent config
- Limited delegation support

### ai-agents.rs
- "One YAML = any agent" philosophy
- YAML for agent definitions/skills + Rust traits for custom tools
- CLI runner: `ai-agents-cli run agent.yaml`
- Closest to open-mpm's skills/agent definition concept
- URL: https://ai-agents.rs

### rust-genai / RLLM
- Multi-provider unified LLM client
- No agent orchestration

---

## Performance Benchmarks (2026)

From benchmarking AutoAgents vs LangChain/LangGraph:

| Metric | AutoAgents (Rust) | LangChain (Python) | Advantage |
|--------|------------------|--------------------|-----------|
| Peak memory | 1,046 MB | 5,146 MB | ~5x less |
| P95 latency | 9,652 ms | 16,891 ms (LangGraph) | ~1.75x faster |
| CPU usage | 29.2% | 64.0% | ~2.2x less |
| Cold start | 4 ms | ~60 ms | ~15x faster |

LLM network round-trips dominate total latency (all frameworks 5.7–7s), but framework overhead is clearly visible at P95. Memory advantages are most striking.

---

## Summary Matrix

| Framework | Tool Calling | Multi-Agent | Subprocess | Skill Defs | Maturity | OpenRouter |
|-----------|-------------|-------------|-----------|-----------|---------|------------|
| Rig | Yes | Via tool | No | No | High | Yes |
| Swiftide | Yes (exp.) | Via tool (exp.) | No | No | Medium | Via OpenAI compat |
| llm-chain | Partial | No | Indirect | No | Low | Via OpenAI compat |
| langchain-rust | Yes | No | Via tool | No | Medium | Via OpenAI compat |
| AutoAgents | Yes | Actor model | WASM | No | New | Yes (native) |
| rmcp | Protocol | Protocol | Yes (stdio) | No | High | N/A |
| ai-agents.rs | Yes | Limited | No | YAML | Unknown | Unknown |

**Key gap across all frameworks:** None provides a PM-orchestrator pattern with subprocess delegation, file-based skill definitions, and message-passing IPC in the claude-mpm style. open-mpm would be novel in this space.

---

## Sources

- [Rig GitHub](https://github.com/0xPlaygrounds/rig)
- [Rig website](https://rig.rs/)
- [Swiftide website](https://swiftide.rs/)
- [llm-chain GitHub](https://github.com/sobelio/llm-chain)
- [langchain-rust GitHub](https://github.com/Abraxas-365/langchain-rust)
- [AutoAgents GitHub](https://github.com/liquidos-ai/AutoAgents)
- [AutoAgents docs](https://liquidos-ai.github.io/AutoAgents/)
- [rmcp / Rust MCP SDK](https://github.com/modelcontextprotocol/rust-sdk)
- [Rust Ecosystem for AI & LLMs (HackMD overview)](https://hackmd.io/@Hamze/Hy5LiRV1gg)
- [Benchmarking AI Agent Frameworks 2026](https://dev.to/saivishwak/benchmarking-ai-agent-frameworks-in-2026-autoagents-rust-vs-langchain-langgraph-llamaindex-338h)
- [ai-agents.rs YAML philosophy](https://ai-agents.rs/blog/why-yaml/)
- [AutoAgents release blog](https://liquidos.ai/blog/autoagents-release/)
