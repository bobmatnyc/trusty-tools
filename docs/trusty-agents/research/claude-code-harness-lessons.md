# Claude Code Harness Architecture: Lessons for open-mpm

**Research date**: 2026-04-22  
**Source**: Reverse-engineering of Claude Code binary at `~/.local/share/claude/versions/2.1.117` (Bun-bundled Mach-O, ~200MB, embedded minified JS)  
**Method**: `strings` extraction with targeted grep patterns; analysis of readable JS fragments and string literals

---

## 1. Source Location

Claude Code ships as a single Bun-compiled Mach-O binary at:
```
~/.local/share/claude/versions/<version>
~/.local/bin/claude -> ~/.local/share/claude/versions/2.1.117
```

The binary contains the full TypeScript/JS application compiled with Bun's bundler. Source is not separately available — all analysis is from the embedded readable strings.

---

## 2. Findings by Concern Area

### 2.1 Tool Dispatch and Result Injection

Claude Code implements a flat tool-calling loop with explicit result injection. The core pattern extracted from the binary:

```javascript
// Tool dispatch: find tool by name, run it, inject result
async function f2K(H, _ = H.messages.at(-1)) {
  if (!_ || _.role !== "assistant" || ...) return null;
  let q = _.content.filter(O => O.type === "tool_use");
  if (q.length === 0) return null;
  return {
    role: "user",
    content: await Promise.all(q.map(async O => {
      let T = H.tools.find($ => ("name" in $ ? $.name : $.mcp_server_name) === O.name);
      if (!T || !("run" in T)) return {
        type: "tool_result", tool_use_id: O.id,
        content: `Error: Tool '${O.name}' not found`, is_error: true
      };
      try {
        let $ = O.input;
        if ("parse" in T && T.parse) $ = T.parse($);
        let A = await T.run($);
        return { type: "tool_result", tool_use_id: O.id, content: A };
      } catch ($) {
        return { type: "tool_result", tool_use_id: O.id, content: ..., is_error: true };
      }
    }))
  };
}
```

**Key observations**:
- Tool lookup by name at dispatch time (not pre-registered at construction)
- Tool has optional `parse` method for input coercion
- Tool has `run` method that returns content
- All tool_use in a response batch are dispatched in `Promise.all` (parallel)
- Errors are returned as `is_error: true` tool_result, NOT thrown — the loop continues

**In open-mpm, apply as**: The `delegate_to_agent` tool should follow this same pattern — the PM's tool dispatcher should handle errors by returning them as structured error results, not panicking. The Rust implementation can batch multiple tool_use calls in parallel using `tokio::join!` or `FuturesUnordered`.

---

### 2.2 System Prompt Construction (Layered Architecture)

The binary reveals a multi-layer system prompt assembly system with named sections and caching:

```
Functions found:
  getSystemPrompt            - main entry point
  before_getSystemPrompt     - hook for pre-processing
  after_getSystemPrompt      - hook for post-processing
  appendSystemPrompt         - add content to prompt
  appendSubagentSystemPrompt - sub-agent specific additions
  appendSystemPromptFile     - inject from file (CLAUDE.md)
  appendSystemPromptFlag     - inject from CLI flag
  overrideSystemPrompt       - full replacement
  customSystemPrompt         - user-provided content
  defaultSystemPrompt        - base content
  longSystemPrompt           - extended version
  getSystemPromptSectionCache       - cache by section name
  setSystemPromptSectionCacheEntry  - update cache entry
  clearSystemPromptSectionState     - reset sections
  excludeDynamicSystemPromptSections - skip dynamic parts
  skipGlobalCacheForSystemPrompt    - bypass cache
  skipSystemPromptPrefix            - omit prefix
  renderedSystemPrompt       - final assembled result
  classifierSystemPromptLength - token counting for classifier
```

The `InstructionsLoaded` hook event fires when CLAUDE.md is loaded.

**Layers in order** (inferred from function names and startup code):
1. `defaultSystemPrompt` — base Claude Code identity
2. `customSystemPrompt` — user's `--system-prompt` flag
3. `appendSystemPromptFile` — CLAUDE.md content (walks up directory tree)
4. `appendSubagentSystemPrompt` — sub-agent-specific additions injected when spawning
5. `before_getSystemPrompt` / `after_getSystemPrompt` — hook injection points

CLAUDE.md is looked for at `./CLAUDE.md` and `../../CLAUDE.md` (walks upward).

**In open-mpm, apply as**:
- Build `SystemPromptBuilder` struct with ordered layers: `base_prompt`, `skills_content`, `project_instructions`, `agent_specific`
- Cache the assembled prompt per-agent-type to avoid reassembling on every turn
- Support file-based injection: scan for `CLAUDE.md` walking up from cwd
- Add `before_assemble` / `after_assemble` extension points as function pointers or trait objects

---

### 2.3 Context Window Management and Compaction

Claude Code uses a `compactionControl` mechanism with explicit token threshold triggering:

```javascript
// From BetaToolRunner class
Nv8 = async function() {
  let _ = Q6(this, C2, "f").params.compactionControl;
  if (!_ || !_.enabled) return false;
  let q = 0;
  // ... sum input_tokens + cache_creation_input_tokens + cache_read_input_tokens + output_tokens
  let K = _.contextTokenThreshold ?? kv8;  // default threshold
  if (q < K) return false;
  let O = _.model ?? Q6(this, C2, "f").params.model;
  // ... trigger compaction using specified model
}
```

The `compactionControl` object has:
- `enabled: boolean`
- `contextTokenThreshold: number` (token count at which to compact)
- `model: string` (which model to use for summarization, can differ from main model)

From message structures, compaction is tracked per-message via `context_management` field:
```javascript
message: {
  ...H.message,
  content: [K],
  context_management: H.message.context_management ?? null
}
```

Hook events `PreCompact` and `PostCompact` fire around compaction.

**In open-mpm, apply as**:
- Track `input_tokens` from each API response in the PM's conversation state
- When token count exceeds threshold (e.g., 80% of model context window), trigger a summarization pass
- Implement `compact_conversation(messages: Vec<Message>, model: &str) -> Result<String>` that calls the LLM with a summarization prompt
- Replace the conversation history with a single "summary" system message + the last N turns
- The threshold and summarization model should be configurable per-agent in the TOML config

---

### 2.4 Sub-Agent Spawning and Lifecycle

Claude Code manages sub-agents as `local_agent` task entries with a state machine:

**Agent states**: `running` → `completed` | `failed` | `killed`

**Agent types** (built-in):
- `general-purpose` — default, all tools
- `planner` — exploration/design only, no edit tools  
- `code-reviewer` — read-only tools
- `summarizer` — text processing
- `fork` — inherits full parent context, no subagent_type needed
- Plus user-defined types from `.claude/agents/*.md` files

**Agent task record**:
```javascript
{
  type: "local_agent",
  status: "running",
  agentId: uuid,
  prompt: string,
  cwd: string,
  selectedAgent: AgentDefinition,
  agentType: string,
  abortController: AbortController,
  retrieved: false,
  isBackgrounded: false,
  pendingMessages: [],
  retain: false,
}
```

**Fork vs. fresh subagent**:
- Fork: inherits full conversation context, triggered by omitting `subagent_type`, uses `permissionMode: "bubble"`, `maxTurns: 200`
- Fresh subagent: zero context, `subagent_type` required, prompt must be self-contained
- Fork avoids filling the parent's context with sub-agent tool output

**Killing agents**:
```javascript
function VF(agentId, taskRegistry) {
  taskRegistry.update(agentId, K => {
    if (K.status !== "running") return K;
    K.abortController?.abort();
    K.unregisterCleanup?.();
    return { ...K, status: "killed", endTime: Date.now(), evictAfter: ... };
  });
}
```

**Permission bubbling**: Sub-agents with `permissionMode: "bubble"` escalate permission requests to the parent rather than prompting the user themselves.

**In open-mpm, apply as**:
- Model sub-agents as state-tracked tasks with UUIDs, not fire-and-forget processes
- Add abort signal propagation: parent PM abort should propagate to child sub-agent processes
- Implement `AgentTask` enum: `Running { pid, abort_tx }` | `Completed { result }` | `Failed { error }` | `Killed`
- The "fork" pattern in open-mpm would be useful for parallel tool calls that don't need to return context to the PM
- Track `cwd` per-agent for git worktree isolation

---

### 2.5 MCP Server Management

MCP server startup happens during the main initialization sequence. Key patterns:

```javascript
// Startup sequence (from main init)
let { servers: kA } = await dH;  // config resolution, awaited at startup
// Two categories of MCP:
let { B3: sdkMcp, mf: stdioMcp } = partition(UJ);
// Parallel connection of all MCP servers:
let M2 = b3_(mf);  // connect stdio MCP servers
let HT = _6.then($ => b3_($));  // connect agent-frontmatter MCP
let oM = Promise.all([M2, HT]).then(([...]) => merged);
```

**MCP errors are separated from settings errors at startup**:
```javascript
let { errors: $6 } = Vp();
let Mq = $6.filter(l9 => !l9.mcpErrorMetadata);  // non-MCP errors only block startup
```

MCP errors are logged but do NOT block startup — the harness starts without the failed MCP server and logs a warning.

**MCP tool registration**: MCP tools are registered with `isMcp: true` flag and are treated as "deferred" tools (not loaded into every prompt by default). They use `ToolSearch` to load schemas on demand.

**MCP logging**: Uses dedicated `logMCPError` / `logMCPDebug` functions buffered in `vGH` before the logger is ready.

**In open-mpm, apply as**:
- Spawn MCP servers at startup in parallel (not lazily per-request)
- Keep an MCP health map: `HashMap<String, McpStatus>` where `McpStatus = Connected | Failed(String) | Reconnecting`
- MCP failures at startup should warn but not abort the PM
- Buffer MCP errors before the logging subsystem is initialized
- Deferred tool loading: don't include all MCP tool schemas in every LLM call; load on-demand when the LLM requests a specific tool

---

### 2.6 Hook System (Pre/PostToolUse)

Claude Code has a rich hook system executed as external processes or inline agents:

**Hook events** (complete list from binary):
```
PreToolUse          PostToolUse         PostToolUseFailure
PermissionDenied    Notification        UserPromptSubmit
UserPromptExpansion SessionStart        SessionEnd
Stop                StopFailure         SubagentStart
SubagentStop        PreCompact          PostCompact
PermissionRequest   Setup               TeammateIdle
TaskCreated         TaskCompleted       Elicitation
ElicitationResult   ConfigChange        WorktreeCreate
WorktreeRemove      InstructionsLoaded  CwdChanged
FileChanged
```

**Hook config structure**:
```javascript
{
  event: "PostToolUse",
  matcher: "Write|Edit",   // regex or tool name pattern
  hooks: [{ command: "ruff format <file>" }],
  source: "projectSettings" | "userSettings" | "pluginHook" | "sessionHook"
}
```

**PreToolUse hook can return**:
- `permissionDecision: "allow" | "deny" | "ask"` — gate tool execution
- `permissionDecisionReason: string` — shown to user
- `updatedInput: object` — modify tool input before execution

**Hook types**:
- `hook_success` — normal completion
- `hook_blocking_error` — prevents tool from running
- `hook_cancelled` — hook was cancelled
- `hook_error_during_execution` — hook script failed
- `hook_non_blocking_error` — logged but tool runs anyway
- `hook_system_message` — injects message into conversation
- `hook_additional_context` — adds context without blocking
- `hook_stopped_continuation` — halts the agent loop
- `hook_deferred_tool` — tool execution deferred

**Settings priority** (lowest to highest): `userSettings` → `projectSettings` → `localSettings` → `pluginHook` → `sessionHook` → `builtinHook`

**Agent hooks**: Hooks can themselves be LLM agents with up to 50 turns, structured output, and `dontAsk` permission mode.

**In open-mpm, apply as**:
- Implement a `HookRegistry` with event-type dispatch
- Support at minimum: `PreToolUse` (for permission gating), `PostToolUse` (for side effects like formatting), `SubagentStart`/`SubagentStop` (for logging/metrics)
- Hook configs in TOML agent files: `[[hooks]] event = "PostToolUse" matcher = "bash" command = "..."`
- Return `PermissionDenied` as a structured tool_result rather than panicking

---

### 2.7 Permission Model

**Permission modes** (from binary):
```
"default"      - ask user for new tool patterns
"acceptEdits"  - auto-accept file edits without asking
"dontAsk"      - allow everything (used by hook agents and sub-agents)
"plan"         - no execution, planning only
```

**Permission rules** are stored in `.claude/settings.json` under `alwaysAllowRules`:
```javascript
{
  session: ["Read(/some/path)", "Bash(ls*)"],
  project: [...],
  user: [...]
}
```

**Tool permission patterns** (from `filePatternTools`, `bashPrefixTools`):
```javascript
filePatternTools: ["Read", "Write", "Edit", "Glob", "NotebookRead", "NotebookEdit"]
bashPrefixTools: ["Bash"]
// File tools: permission uses glob pattern match against file path
// Bash tools: permission uses prefix match against command string
```

**Custom validation per tool**:
```javascript
WebSearch: (H) => {
  if (H.includes("*")) return { valid: false, error: "no wildcards" };
  return { valid: true };
}
```

**In open-mpm, apply as**:
- Implement `PermissionMode` enum: `Ask | AcceptEdits | DontAsk | Plan`
- Sub-agents run in `DontAsk` mode by default; PM runs in `Ask` mode
- Store allow-lists per-session: `HashMap<String, Vec<PermissionRule>>`
- `PermissionRule` can be `FilePath(glob)`, `BashPrefix(prefix)`, or `Wildcard`

---

### 2.8 Streaming vs. Non-Streaming

The binary shows streaming is always used when max_tokens exceeds ~16K. The main loop uses async iterator pattern:

```javascript
for await (let U of cV({ messages, systemPrompt, ... })) {
  if (U.type === "stream_event" || U.type === "stream_request_start") continue;
  if (U.type === "assistant") { /* process turn */ }
  // ...
}
```

The `stream_event` and `stream_request_start` types are filtered out — only `assistant` and `user` turn messages are processed in the application layer. Streaming is an implementation detail of the transport layer.

**In open-mpm, apply as**:
- Always stream (the `async-openai` crate supports streaming via `create_stream()`)
- Buffer streamed chunks into complete messages before dispatching to tool handlers
- Track token counts from `usage` field on the final streamed chunk (not per-delta)
- The sub-agent IPC via NDJSON naturally fits: stream response chunks to stdout as they arrive, then send the final `result` message

---

### 2.9 Conversation History and Context Compaction

Each assistant message carries a `context_management` metadata field that Claude Code uses to track compaction state. The message normalization code (function `rP`) handles multi-content messages by splitting them into single-content normalized messages.

**Message types tracked**:
- `assistant` — LLM output (may contain multiple tool_use blocks)
- `user` — human input OR tool_result injection
- `progress` — streaming progress events (filtered before API calls)
- `attachment` — hook results, system messages
- `system` — local command results, API errors

Attachments (hook results) are interleaved into the message list between tool_use and tool_result messages for display, but are **stripped before sending to the API**.

The `fP5` function shows how attachments are removed from the message list before API submission:
```javascript
// Strip attachment messages, keep tool_use → tool_result pairs intact
function fP5(messages, stripIsVirtual = false) { ... }
```

**In open-mpm, apply as**:
- Maintain two parallel lists: `display_messages` (full, for TUI) and `api_messages` (stripped, for LLM calls)
- Or maintain one canonical list and filter at API call time
- Never include `progress` or `attachment` type messages in API payloads
- For context compaction: replace older messages with a summary, but keep the system prompt and the most recent N tool-call/result pairs intact

---

### 2.10 Retry and Error Handling

The binary shows explicit retry with exponential backoff for network calls:
```javascript
const cB9 = [2000, 4000, 8000, 16000];  // delay sequence in ms
const Le6 = cB9.length;  // max 4 retries
async function DU_(url, config) {
  let q;
  for (let K = 0; K <= Le6; K++) try {
    return await axios.get(url, config);
  } catch (O) {
    if (q = O, !isTransientNetworkError(O)) throw O;  // only retry transient errors
    if (K >= Le6) throw O;
    let T = cB9[K] ?? 2000;
    await sleep(T);
  }
  throw q;
}
```

Transient errors: `!response` (network failure) OR `response.status >= 500`.

**In open-mpm, apply as**:
- Use `tokio::time::sleep` + retry loop for OpenRouter API calls
- Only retry on 5xx or network errors, NOT on 4xx (which indicate client bugs)
- Backoff: `[2s, 4s, 8s, 16s]` (4 retries)
- Wrap with `anyhow` context at each retry boundary for debugging

---

### 2.11 Workflow Phases: Flat Tool Loop vs. Prescriptive

Claude Code does NOT implement a prescriptive workflow engine. It is a **flat tool-calling loop** with emergent behavior guided by system prompt instructions and hook events.

However, it provides "prescriptive nudges" via:
1. **Specialized agent types** with role-restricted tool sets (planner gets read-only tools, code-reviewer gets read-only tools)
2. **System prompt enforcement**: "Your role is EXCLUSIVELY to search and analyze existing code. You do NOT have access to file editing tools"
3. **Hook-based enforcement**: `PreToolUse` hooks can deny tool calls based on current workflow state
4. **Verification agent**: The binary contains explicit prompting to spawn a verification subagent after 3+ completed tasks — a prescriptive workflow pattern baked into the PM's system prompt

```
NOTE: You just closed out 3+ tasks and none of them was a verification step. 
Before writing your final summary, spawn the verification agent 
(subagent_type="<verifier>"). You cannot self-assign PARTIAL by listing 
caveats in your summary — only the verifier issues a verdict.
```

**In open-mpm, apply as**:
- open-mpm's prescriptive workflow (Research → Plan → Code → QA → Observe) can be implemented as:
  a. Separate agent types with appropriate tool restrictions per phase
  b. System prompt with explicit phase descriptions and tool constraints
  c. Hook-based enforcement at transition points (e.g., PreToolUse denies edit tools during Research phase)
- The PM's `delegate_to_agent` tool can enforce phase order: PM state machine tracks current phase and only delegates to phase-appropriate agents

---

## 3. Top 7 Actionable Lessons for open-mpm

### Lesson 1: Structured Error Injection, Not Panic
**Finding**: Tool errors are returned as `{ type: "tool_result", is_error: true, content: "..." }` — the loop continues. The LLM decides how to handle the error.

**Apply in open-mpm**: In `src/tools/`, implement `ToolResult::Error(String)` variant. The PM loop should serialize this as a tool_result and continue to the next LLM turn. Only panic for unrecoverable harness errors, never for tool execution failures.

---

### Lesson 2: System Prompt Layered Builder with Section Caching
**Finding**: Claude Code uses 10+ named injection points assembled in order, with per-section caching keyed by section name.

**Apply in open-mpm**: Create `SystemPromptBuilder` in `src/agents/`:
```rust
pub struct SystemPromptBuilder {
    base: String,           // from agent TOML
    skills: Vec<String>,    // from config/skills/*.md  
    project_instructions: Option<String>,  // CLAUDE.md equivalent
    subagent_additions: Option<String>,    // injected per-spawn
}
impl SystemPromptBuilder {
    pub fn build(&self) -> String { ... }  // assemble in order
}
```
Cache the result in `Arc<RwLock<HashMap<AgentType, String>>>`.

---

### Lesson 3: Token Threshold Compaction
**Finding**: When token count (input + cache_creation + cache_read + output) exceeds `contextTokenThreshold`, Claude Code triggers a summarization pass using a (possibly different) model.

**Apply in open-mpm**: Add to agent TOML:
```toml
[context]
compaction_threshold = 80000  # tokens
compaction_model = "anthropic/claude-haiku-4-5"  # cheaper model for summarization
```
In the PM loop, after each turn: `if total_tokens > threshold { compact().await }`.

---

### Lesson 4: Agent State Machine with Abort Propagation
**Finding**: Each sub-agent is a state-tracked task (`running` → `completed`/`failed`/`killed`) with an `AbortController` that propagates cancellation from parent to child.

**Apply in open-mpm**: Model sub-agent processes as:
```rust
pub struct AgentTask {
    pub id: Uuid,
    pub status: AgentStatus,
    pub process: tokio::process::Child,
    pub abort_tx: tokio::sync::oneshot::Sender<()>,
}
// PM SIGINT handler: abort_tx.send(()) for all running tasks
```
This prevents zombie sub-agent processes on PM termination.

---

### Lesson 5: Deferred Tool Loading
**Finding**: MCP tools are registered as "deferred" — their schemas are NOT included in every LLM prompt. A `ToolSearch` tool is included instead, and the LLM requests specific tool schemas on-demand.

**Apply in open-mpm**: For large tool sets, implement lazy tool loading:
- Always include a `list_available_tools` tool in every prompt
- Only include full tool schemas for tools the LLM has explicitly requested in this session
- Reduces prompt token overhead significantly when agents have 20+ tools

---

### Lesson 6: Fork vs. Fresh Sub-Agent Distinction
**Finding**: "Fork" inherits full parent context and is used to offload work without polluting the parent's context window. "Fresh subagent" has zero context and requires a self-contained briefing.

**Apply in open-mpm**: Implement two delegation modes:
- `DelegateMode::Fresh` — spawn sub-agent with only the task description (current behavior)
- `DelegateMode::Fork` — spawn sub-agent with the PM's current conversation summary injected as context
- PM system prompt should instruct: "Fork for independent parallel research; use fresh subagent for isolated tasks that don't need conversation history"

---

### Lesson 7: Hook-Based Prescriptive Workflow Enforcement
**Finding**: Claude Code enforces workflow rules not through a state machine but through system prompt nudges + hook-based tool gating. The `PreToolUse` hook can `deny` tool execution, creating hard workflow constraints.

**Apply in open-mpm**: Implement the Research→Plan→Code→QA→Observe workflow as:
1. **Agent type restrictions**: Research agent has only `Bash(grep|find)`, `Read`, `Glob`; Code agent adds `Write`, `Edit`
2. **PM state tracking**: `enum WorkflowPhase { Research, Plan, Code, QA, Observe }`
3. **Phase-gated delegation**: PM's tool dispatch checks current phase before spawning:
   ```rust
   if phase == Phase::Research && agent_type == AgentType::CodeEngineer {
       return Err("Code agent not available during Research phase");
   }
   ```
4. **Explicit phase transition tool**: Add `advance_workflow_phase(reason: String)` tool that moves the PM forward, creating an audit trail

---

## 4. Architecture Diagram: Claude Code vs. open-mpm Mapping

```
CLAUDE CODE                           OPEN-MPM (current/target)
─────────────────────────────         ───────────────────────────────
BetaToolRunner                   ←→   PM loop in src/main.rs
  compactionControl                    TODO: token threshold compaction
  messages: Vec<Message>               messages: Vec<ChatCompletionMessage>
  tools: Vec<Tool>                     tools: Vec<Tool> (delegate_to_agent)
  
getSystemPrompt() layers         ←→   SystemPromptBuilder TODO
  defaultSystemPrompt                  agent.system_prompt.content (TOML)
  appendSystemPromptFile               TODO: CLAUDE.md walk-up
  appendSubagentSystemPrompt           TODO: per-spawn injection

f2K() tool dispatch              ←→   src/tools/ dispatch
  parallel Promise.all                 FuturesUnordered or join!
  is_error: true on failure            ToolResult::Error variant TODO

local_agent task registry        ←→   TODO: AgentTask state machine
  status: running/done/failed          AgentStatus enum
  abortController                      oneshot::Sender<()>

MCP server startup               ←→   N/A (open-mpm uses subprocess IPC)
  parallel b3_() connections           but: parallel sub-agent spawn TODO

Hook system                      ←→   TODO: HookRegistry
  PreToolUse / PostToolUse             Simple: just log for now
  permissionDecision: deny             Add: phase-gate enforcement

permissionMode                   ←→   TODO: PermissionMode enum
  dontAsk (sub-agents)                 Sub-agents: auto-approve all
  ask (interactive PM)                 PM: interactive or config-based

Streaming LLM calls              ←→   async-openai create_stream()
  filter stream_event types            Buffer deltas, process on finish
  context_management metadata          TODO: track token usage per turn
```

---

## 5. Files Referenced

- Binary: `/Users/masa/.local/share/claude/versions/2.1.117`
- Research output: `/Users/masa/Projects/open-mpm/docs/research/claude-code-harness-lessons.md`
- Related docs: `subprocess-ipc-patterns.md`, `agent-delegation-patterns.md`, `workflow-engine-design.md`

---

## 6. Open Questions for Further Research

1. **How does Claude Code inject skills content?** The `InstructionsLoaded` event fires when CLAUDE.md loads, but the skills injection mechanism for `~/.claude/skills/` files was not fully traced.

2. **What triggers compaction vs. truncation?** The `compactionControl` threshold is clear, but whether truncation (dropping old messages) is ever used as a fallback was not confirmed.

3. **Agent frontmatter MCP**: The startup code shows a second MCP connection path for "agent frontmatter" MCP — these appear to be per-agent MCP server configs, distinct from global settings. Relevance to open-mpm's TOML config approach should be investigated.

4. **The `cV()` main generator**: The core `for await (let U of cV(...))` loop was identified but its full implementation was not extractable from the minified binary. This is the central orchestration function.
