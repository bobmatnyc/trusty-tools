# API Reference

This document covers the public Rust APIs, all configuration formats, and the
complete CLI flags reference for open-mpm.

---

## CLI Flags Reference

open-mpm is a single binary that dispatches based on flags.

```
open-mpm [FLAGS]
```

| Flag | Mode | Description |
|---|---|---|
| `--ctrl` | CTRL REPL | Interactive multi-project session manager (also the default) |
| `--pm` | PM | Single-shot PM orchestrator; reads one line from stdin |
| `--agent <name>` | Sub-agent | Read one NDJSON Task from stdin, emit one Result, exit |
| `--direct <name>` | Direct | Bypass PM LLM, send task to sub-agent directly |
| `--workflow <name>` | Workflow | Run `config/workflows/<name>.json` |
| `--task <text>` | Inline task | Inline task string; used with `--direct` / `--workflow` |
| `--task-file <path>` | Task file | Read task from file; used with `--direct` / `--workflow` |
| `--out-dir <dir>` | Output dir | Sandbox for `write_file` and file extraction |
| `--reindex` | Index | Full re-index of the working tree, then exit |
| `--watch` | Index | Live file-watcher; blocks until killed |
| `--check-orphans` | Diagnostics | Print tracked sub-agent PIDs and their live status |
| `--clear-sessions` | Sessions | Clear in-memory agent session history |
| `--reinit` | Init | Force project re-initialization and memory seeding |
| `--version`, `-V` | Version | Print `open-mpm vX.Y.Z (<sha>) build #N` and exit |

### CLI subcommands

```bash
# Search the code index
open-mpm code search "<query>"

# Search the memory (turn history) index
open-mpm memory search "<query>"

# Run a raw memory operation
open-mpm memory run "<query>"
```

---

## Agent TOML Configuration

Agent configs live at `config/agents/<name>.toml`. The path can be overridden
via `OPEN_MPM_CONFIG_DIR`.

### Full schema

```toml
[agent]
# Required. Short identifier used for subprocess invocation and IPC.
name = "python-engineer"

# Required. Semantic role (not currently used for routing; informational).
role = "engineer"

# Required. Model string in OpenRouter format or bare Anthropic model name.
# Resolution priority (highest first):
#   1. OPEN_MPM_MODEL_<UPPER_SNAKE> env var
#   2. [llm].model_override field (below)
#   3. This field
#   4. OPEN_MPM_DEFAULT_MODEL env var
#   5. Hardcoded fallback: "anthropic/claude-sonnet-4-6"
model = "anthropic/claude-sonnet-4-6"

# Required. Human-readable description (used in PM system prompt agent list).
description = "Python software engineer"

# Optional. Whether to retain conversation history across multiple calls
# within one workflow run. Default: false.
persistent_session = false

# Optional. Which runner implementation to use for this agent.
# "subprocess" (default): re-invokes the open-mpm binary with --agent <name>.
# "claude-code": spawns the claude CLI with OAuth token auth.
# "inline": reserved for future in-process runners.
runner = "subprocess"

[llm]
# Required. Sampling temperature (0.0–1.0).
temperature = 0.2

# Required. Maximum output tokens per request.
max_tokens = 8192

# Optional. Override the model for this agent only (takes precedence over
# [agent].model but not over OPEN_MPM_MODEL_<AGENT> env var).
# model_override = "anthropic/claude-haiku-4"

# Optional. Enable Anthropic ephemeral prompt caching on the system message.
# Default: true. Ignored for non-Anthropic models.
enable_prompt_caching = true

# Optional. Maximum number of LLM turns in the tool-calling loop.
# Default: 20. Ignored for single-shot (no-tool) agents.
max_turns = 20

# Optional. Override max_turns at invocation time via OPEN_MPM_MAX_TURNS env var.
# The env var takes precedence over this field.

# Optional. Controls the `tool_choice` API field.
# "auto" (default): model decides whether to call a tool.
# "any": model must always call some tool (use with use_finish_task = true).
# "none": no tools.
tool_choice = "auto"

# Optional. Auto-inject the `finish_task` terminal tool and exit the loop when
# the model calls it. Needed when tool_choice = "any". Default: false.
use_finish_task = false

# Optional. Route LLM calls directly to api.anthropic.com instead of OpenRouter.
# Requires ANTHROPIC_API_KEY. Do not combine with CLAUDE_CODE_OAUTH_TOKEN.
# Default: false.
use_anthropic_direct = false

[system_prompt]
# Required. The agent's base system prompt.
content = """
You are a Python engineer. Implement the task fully.
"""

# Optional. Named skill files to resolve and append to the system prompt.
# Skills are looked up in the skill registry (config/skills/, ~/.open-mpm/skills/, etc.)
# skills = ["tdd", "python-style"]

[tools]
# Optional. Allowlist of tool names this agent may call.
# Absent = no restriction (all registered tools callable).
# When present, dispatch_gated rejects any tool not in this list.
# allowed = ["read_file", "write_file", "grep_files"]

# Optional. Native typed tool flags (default shown).
[tools.native]
# Register SearchCodeTool, SearchMemoryTool, SearchSkillsTool.
native_search = true

# Register StoreMemoryTool, RetrieveMemoryTool, ListMemoryKeysTool.
native_memory = true

# Register ticketing tools (CreateTicket, GetTicket, CloseTicket, etc.).
# Requires [ticketing] to also be configured.
native_ticketing = false

[ticketing]
# Optional. Ticketing provider config for native_ticketing = true agents.
# All fields optional; missing fields fall back to env vars.
# provider = "github"             # "github" | "jira" | "linear"
# github_token = ""               # or env GITHUB_TOKEN
# github_repo = "owner/repo"      # or env GITHUB_REPO
# jira_url = "https://org.atlassian.net"
# jira_email = ""
# jira_token = ""
# jira_project = "PROJ"
# linear_api_key = ""
# linear_team_id = ""
```

### Bundled agents

| Agent | Role | Default Tools |
|---|---|---|
| `pm` | Orchestrator | `delegate_to_agent` |
| `research-agent` | Research | `web_search`, `fetch_url`, `memory_recall`, `vector_search`, `memory_search`, `load_skill`, `list_skills`, `phase_audit` |
| `plan-agent` | Architecture | `memory_recall`, `vector_search`, `memory_search`, `write_file`, `load_skill`, `list_skills`, `phase_audit` |
| `code-agent` / `engineer` | Implementation | `read_file`, `list_dir`, `grep_files`, `write_file`, `load_skill`, `list_skills`, `phase_audit` |
| `qa-agent` | Quality assurance | `web_search`, `fetch_url`, `memory_recall`, `vector_search`, `memory_search`, `shell_exec`, `load_skill`, `list_skills`, `phase_audit` |
| `observe-agent` | Synthesis | skill tools |
| `explorer-agent` | Read-only exploration | `web_search`, `fetch_url`, `memory_recall`, `vector_search`, `read_file`, `list_dir`, `grep_files`, `load_skill`, `list_skills`, `phase_audit` |
| `local-ops-agent` | Shell operations | `shell_exec` (allowlisted), `read_file`, `list_dir`, `grep_files`, `finish_task`, skill tools |
| `docs-agent` | Documentation | `read_file`, `list_dir`, `grep_files`, `write_file`, `finish_task`, skill tools |
| `ctrl` | CTRL actor | Internal |

---

## Workflow JSON Configuration

Workflow configs live at `config/workflows/<name>.json`.

### Full schema

```jsonc
{
  "name": "prescriptive",
  "description": "Research -> Plan -> Code -> QA -> Observe",

  // Optional. Auto-commit and push after a successful run.
  "auto_push": {
    "enabled": false,
    "version_bump": "patch",   // "patch" | "minor" | "none"
    "commit_message_template": "feat(workflow): {{workflow}} build {{build}} — {{task_preview}}",
    "push_remote": "origin",
    "push_branch": "main"
  },

  // Optional. Automatic GitHub issue lifecycle management.
  "ticket_management": {
    "enabled": false,
    "repo": "owner/repo",            // Required when enabled = true
    "assignee": "username",
    "milestone": "v1.0.0",
    "labels": ["poc"],
    "auto_relate": true,             // Search and cross-link related issues
    "phase_comments": true,          // Comment on issue after each phase
    "close_on_success": true         // Close issue on successful completion
  },

  "phases": [
    {
      // Required. Phase identifier. Used as template variable in later phases
      // as {{phase_name}}.
      "name": "research",

      // Required. Agent name; must resolve to config/agents/<agent>.toml
      // (or a claude-mpm agent in .claude/agents/).
      "agent": "research-agent",

      // Optional. Model override for this phase only.
      // Priority: phase model_override > agent TOML model_override > agent TOML model.
      "model_override": "anthropic/claude-sonnet-4-6",

      // Required. Template expanded against WorkflowContext before the task
      // is sent to the agent. Available variables:
      //   {{task}}          - original user task text
      //   {{out_dir}}       - absolute path to the --out-dir
      //   {{<phase_name>}}  - output (or summary) of a prior named phase
      "context_template": "{{task}}",

      // Optional. When true, extract ## File: <path> sections from this
      // phase's output and write them to out_dir BEFORE the next phase runs.
      // Required for code phases that the QA phase needs to test.
      "produces_files": false,

      // Optional. When true, skip this phase entirely. Useful for optional
      // phases (e.g. docs) that are off by default.
      "skip": false,

      // Optional. List of skill names or ["auto"] to inject for this phase.
      // "auto" triggers language/framework detection from the project and task.
      // Absent = use agent TOML's system_prompt.skills field.
      "skills": null,

      // Optional. Run multiple sub-agents concurrently for this phase.
      "parallel_subtasks": [
        {
          "label": "backend",        // Short ID; used for worktree dir names
          "task_suffix": "Focus on the backend API layer."
        },
        {
          "label": "frontend",
          "task_suffix": "Focus on the React frontend."
        }
      ],

      // Optional. When true and parallel_subtasks is set, each subtask runs
      // in a dedicated git worktree to prevent file conflicts. Default: false.
      "worktree_protection": false
    }
  ]
}
```

### Template variables

| Variable | Expands to |
|---|---|
| `{{task}}` | Original user task text |
| `{{out_dir}}` | Absolute path passed as `--out-dir` |
| `{{<phase_name>}}` | `summary` field of the named phase (falls back to first 500 chars of content) |

---

## Wave Loop (assignments.json)

When the plan-agent writes `assignments.json` to `out_dir`, the code phase
switches from a single monolithic sub-agent to a per-file wave loop.

**Schema** (`out_dir/assignments.json`):

```jsonc
{
  "error_convention": "exceptions",   // Optional global hint for code-agents
  "waves": [
    {
      "wave": 1,                       // 1-indexed ordinal; must be sequential
      "files": [
        {
          "path": "src/util.py",       // Relative to out_dir; must not contain ".."
          "stub": "stubs/util.py",     // Relative path to stub file, or null
          "purpose": "Shared helpers", // One-line description injected into prompt
          "depends_on": [],            // Paths from strictly earlier waves
          "max_lines": 120             // Optional line budget hint
        }
      ]
    },
    {
      "wave": 2,
      "files": [
        {
          "path": "src/main.py",
          "stub": "stubs/main.py",
          "purpose": "Entrypoint",
          "depends_on": ["src/util.py"],
          "max_lines": null
        }
      ]
    }
  ]
}
```

Validation rules enforced at load time:

- Waves must have sequential 1-indexed ordinals
- File paths must be relative (no `/` prefix, no `..` components)
- No duplicate file paths across all waves
- `depends_on` entries must reference a path in a strictly earlier wave

---

## IPC Message Format (NDJSON)

The PM and sub-agent communicate over stdin/stdout using newline-delimited JSON.
Each message is one JSON object per line.

### PM to sub-agent (Task)

```json
{
  "type": "task",
  "id": "550e8400-e29b-41d4-a716-446655440000",
  "task": "Write a Python script that...",
  "history": [
    {"role": "user", "content": "prior turn"},
    {"role": "assistant", "content": "prior response"}
  ],
  "session_reset": false
}
```

`history` and `session_reset` are optional and omitted when absent.

### Sub-agent to PM (Result)

```json
{
  "type": "result",
  "id": "550e8400-e29b-41d4-a716-446655440000",
  "content": "Full agent output text (may include ## File: sections)",
  "summary": "Concise summary extracted from ## Summary section",
  "usage": {
    "prompt_tokens": 1500,
    "completion_tokens": 800,
    "cache_read_tokens": 200,
    "cache_creation_tokens": 100
  },
  "status": "success"
}
```

`summary` and `usage` are optional.

### Sub-agent to PM (Error)

```json
{
  "type": "error",
  "id": "550e8400-e29b-41d4-a716-446655440000",
  "error": "agent 'python-engineer' failed: ...",
  "status": "error"
}
```

---

## Core Rust APIs

### `AgentConfig` (`src/agents/mod.rs`)

```rust
impl AgentConfig {
    /// Load from a TOML file path.
    pub fn load(path: &Path) -> Result<Self>

    /// Load by short name (e.g. "python-engineer").
    /// Honors OPEN_MPM_CONFIG_DIR env var; falls back to config/agents/.
    pub fn by_name(name: &str) -> Result<Self>

    /// Async variant of by_name (uses tokio::fs).
    pub async fn by_name_async(name: &str) -> Result<Self>

    /// Load from an explicit directory, bypassing OPEN_MPM_CONFIG_DIR.
    pub async fn load_from_dir(name: &str, dir: &Path) -> Result<Self>
}
```

**Model resolution order** (highest priority first):

1. `OPEN_MPM_MODEL_<UPPER_SNAKE>` env var (e.g. `OPEN_MPM_MODEL_CODE_AGENT`)
2. `[llm].model_override` in agent TOML
3. `[agent].model` in agent TOML
4. `OPEN_MPM_DEFAULT_MODEL` env var
5. Hardcoded fallback: `"anthropic/claude-sonnet-4-6"`

### `ToolRegistry` (`src/tools/mod.rs`)

```rust
impl ToolRegistry {
    pub fn new() -> Self

    /// Register a tool. Panics on duplicate names in debug builds.
    pub fn register(&mut self, tool: Arc<dyn ToolExecutor>)

    pub fn contains(&self, name: &str) -> bool

    /// Dispatch by name. Returns ToolResult::Error when name not found.
    pub async fn dispatch(&self, name: &str, args: Value) -> ToolResult

    /// Dispatch with a per-agent allowlist. Rejects tools not in the list.
    pub async fn dispatch_gated(
        &self,
        name: &str,
        args: Value,
        allowed: Option<&[String]>
    ) -> ToolResult

    /// Raw JSON schemas for all registered tools.
    pub fn schemas(&self) -> Vec<Value>

    /// async-openai typed tool definitions.
    pub fn openai_tools(&self) -> Result<Vec<ChatCompletionTool>>
}
```

### `ToolExecutor` trait (`src/tools/traits.rs`)

```rust
#[async_trait]
pub trait ToolExecutor: Send + Sync {
    fn name(&self) -> &str;
    fn schema(&self) -> Value;    // Full OpenAI-compatible {"type":"function",...}
    async fn execute(&self, args: Value) -> ToolResult;
}
```

### `ToolResult` (`src/tools/traits.rs`)

```rust
pub enum ToolResult {
    Success(String),
    Error { message: String, recoverable: bool },
}

impl ToolResult {
    pub fn ok(s: impl Into<String>) -> Self      // recoverable success
    pub fn err(msg: impl Into<String>) -> Self   // recoverable error
    pub fn fatal(msg: impl Into<String>) -> Self // non-recoverable error
    pub fn is_error(&self) -> bool
    pub fn is_fatal(&self) -> bool
    pub fn content(&self) -> &str
}
```

### `AgentRunner` trait (`src/tools/traits.rs`)

```rust
#[async_trait]
pub trait AgentRunner: Send + Sync {
    async fn run(&self, agent_name: &str, task: &str) -> Result<AgentOutput>;

    // Optional: forward prior session history (default impl ignores history)
    async fn run_with_history(
        &self,
        agent_name: &str,
        task: &str,
        history: &[HistoryMessage],
    ) -> Result<AgentOutput>;

    // Optional: forward per-invocation RunContext (default impl ignores ctx)
    async fn run_with_context(
        &self,
        agent_name: &str,
        task: &str,
        ctx: &RunContext,
    ) -> Result<AgentOutput>;
}
```

### `RunContext` (`src/tools/traits.rs`)

```rust
pub struct RunContext {
    /// Scope write_file to this path (wave loop per-file invocations).
    pub assigned_file: Option<PathBuf>,

    /// Cap max_turns for this invocation.
    pub max_turns_override: Option<u32>,

    /// Subprocess working directory.
    pub working_dir: Option<PathBuf>,

    /// Model override; sourced from workflow phase `model_override` field.
    pub model_override: Option<String>,
}
```

### `AgentOutput` (`src/tools/traits.rs`)

```rust
pub struct AgentOutput {
    pub content: String,           // Full agent output text
    pub summary: Option<String>,   // Extracted ## Summary section
    pub usage: TokenUsage,         // Aggregated token counts
}
```

### `IpcMessage` (`src/ipc/mod.rs`)

```rust
pub enum IpcMessage {
    Task   { id: String, task: String, history: Option<Vec<HistoryMessage>>, session_reset: Option<bool> },
    Result { id: String, content: String, summary: Option<String>, usage: Option<TokenUsage>, status: String },
    Error  { id: String, error: String, status: String },
}

impl IpcMessage {
    pub fn new_task(task: impl Into<String>) -> Self
    pub fn new_task_with_history(task: impl Into<String>, history: Vec<HistoryMessage>) -> Self
    pub fn new_result(id: impl Into<String>, content: impl Into<String>) -> Self
    pub fn new_result_with_summary(id, content, summary: Option<String>) -> Self
    pub fn new_result_full(id, content, summary: Option<String>, usage: Option<TokenUsage>) -> Self
    pub fn new_error(id: impl Into<String>, error: impl Into<String>) -> Self
}

pub fn serialize_message(msg: &IpcMessage) -> Result<String>  // Returns JSON + "\n"
pub fn parse_message(line: &str) -> Result<IpcMessage>

// Parse ## File: <path> sections from LLM output
pub fn extract_files_from_content(content: &str) -> Vec<(PathBuf, String)>

// Extract ## Summary section (or first 500 chars fallback)
pub fn extract_summary(content: &str) -> String
```

### LLM Client (`src/llm/mod.rs`)

```rust
pub fn create_client() -> Result<Client<OpenAIConfig>>

pub async fn chat(
    client: &Client<OpenAIConfig>,
    model: &str,
    system_prompt: &str,
    user_message: &str,
    temperature: f32,
    max_tokens: u32,
    tools: Vec<ChatCompletionTool>,
) -> Result<ChatResponse>

pub async fn chat_with_tools_gated(
    client: &Client<OpenAIConfig>,
    model: &str,
    adapter: &dyn ModelAdapter,
    initial_messages: Vec<ChatCompletionRequestMessage>,
    registry: Arc<ToolRegistry>,
    allowed_tools: Option<Vec<String>>,
    temperature: f32,
    max_tokens: u32,
    max_turns: u32,
    enable_prompt_caching: bool,
    tool_choice: Option<serde_json::Value>,
    use_finish_task: bool,
    use_anthropic_direct: bool,
) -> Result<(String, TokenUsage)>

pub fn should_retry_plain_text_turn(
    consecutive_no_tool_turns: u32,
    turn: u32,
    max_turns: u32,
) -> bool
```

### `WorkflowDef` (`src/workflow/config.rs`)

```rust
impl WorkflowDef {
    pub fn load(path: &Path) -> Result<Self>
    pub fn validate(&self) -> Result<()>
}

impl Assignments {
    pub fn load(out_dir: &Path) -> Option<Self>
    pub fn validate(&self) -> Result<()>
    pub fn validate_file_path(path: &str) -> Result<()>
}
```

### `WorkflowEngine` (`src/workflow/engine.rs`)

```rust
impl WorkflowEngine {
    pub fn new(runner: Arc<dyn AgentRunner>, workflows_dir: PathBuf) -> Self

    // Builder methods
    pub fn with_build(self, build_num: u64) -> Self
    pub fn with_perf_dir(self, dir: Option<PathBuf>) -> Self
    pub fn with_indexer(self, indexer: Option<HistoryIndexer>) -> Self
    pub fn with_skill_registry(self, registry: Option<Arc<SkillRegistry>>) -> Self
    pub fn with_skills_loader(self, loader: Option<Arc<SkillsLoader>>) -> Self
    pub fn with_init_context(self, ctx: Option<InitContext>) -> Self
    pub fn with_user_memory(self, suffix: Option<String>) -> Self
    pub fn with_ticket_manager(self, tm: TicketManager) -> Self

    pub async fn run(
        &mut self,
        workflow_name: &str,
        task: String,
        out_dir: Option<PathBuf>,
    ) -> Result<WorkflowContext>
}
```

### `TokenUsage` (`src/perf.rs`)

```rust
pub struct TokenUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub cache_read_tokens: u32,     // Anthropic-specific; 0 for other providers
    pub cache_creation_tokens: u32, // Anthropic-specific; 0 for other providers
}

impl TokenUsage {
    pub fn new(prompt: u32, completion: u32, cache_read: u32, cache_creation: u32) -> Self
    pub fn add(&mut self, other: &TokenUsage)
}
```

### `ModelAdapter` trait (`src/llm/adapter.rs`)

```rust
pub trait ModelAdapter: Send + Sync {
    fn provider(&self) -> Provider;
    fn uses_native_format(&self) -> bool;
    fn tool_choice_any(&self) -> Option<Value>;
    fn tool_choice_auto(&self) -> Option<Value>;
    fn inject_cache_control(&self, raw: &mut Value, active: bool);
    fn parse_usage(&self, json: &Value) -> TokenUsage;
    fn api_endpoint(&self, use_direct: bool) -> ApiEndpoint;
}

pub fn adapter_for_model(model: &str) -> Box<dyn ModelAdapter>

pub enum Provider { Anthropic, OpenAI, Generic }
```

---

## Built-in Tools Reference

### Filesystem tools

| Tool | Name | Description | Args |
|---|---|---|---|
| `ReadFileTool` | `read_file` | Read a file's contents | `{"path": "relative/path"}` |
| `ListDirTool` | `list_dir` | List directory contents | `{"path": "relative/path"}` |
| `GrepFilesTool` | `grep_files` | Search files for a pattern | `{"pattern": "regex", "path": "dir", "file_glob": "*.rs"}` |
| `WriteFileTool` | `write_file` | Write content to a file atomically | `{"path": "relative/path", "content": "..."}` |

`WriteFileTool` scopes writes to the configured `out_dir`. When
`OPEN_MPM_ASSIGNED_FILE` is set (wave loop), only the assigned path is writable.

### Shell tools

| Tool | Name | Description |
|---|---|---|
| `ShellExecTool` (qa-agent) | `shell_exec` | Run shell commands; returns stdout/stderr |
| `ShellExecTool` (local-ops) | `shell_exec` | Allowlisted shell for local-ops-agent |

### Web tools

| Tool | Name | Args |
|---|---|---|
| `BraveSearchTool` | `web_search` | `{"query": "...", "n": 5}` |
| `FetchUrlTool` | `fetch_url` | `{"url": "https://..."}` |

`BraveSearchTool` degrades gracefully when `BRAVE_API_KEY` is unset (returns
a ToolResult::Error that the LLM can handle).

### Memory tools

| Tool | Name | Description |
|---|---|---|
| `KuzuRecallTool` | `memory_recall` | Query the kuzu knowledge graph |
| `VectorSearchTool` | `vector_search` | ANN search in the local code index |
| `MemorySearchTool` | `memory_search` | Hybrid vector+BM25 over the turn history log |

### Skill tools

| Tool | Name | Args |
|---|---|---|
| `SkillLoaderTool` | `load_skill` | `{"name": "tdd"}` |
| `SkillListTool` | `list_skills` | `{}` |

### Workflow control tools

| Tool | Name | Description |
|---|---|---|
| `PhaseAuditTool` | `phase_audit` | Signal phase completion (used by plan/code agents) |
| `FinishTaskTool` | `finish_task` | Terminal tool for `tool_choice = "any"` agents |
| `DelegateToAgentTool` | `delegate_to_agent` | PM: spawn a sub-agent for a task |

### Native typed tools (opt-in via `[tools.native]`)

| Tool | Name | Description |
|---|---|---|
| `SearchCodeTool` | `search_code` | Typed code search |
| `SearchMemoryTool` | `search_memory` | Typed memory search |
| `SearchSkillsTool` | `search_skills` | Typed skills search |
| `StoreMemoryTool` | `store_memory` | Write a key-value memory entry |
| `RetrieveMemoryTool` | `retrieve_memory` | Read a memory entry by key |
| `ListMemoryKeysTool` | `list_memory_keys` | List all stored memory keys |
| `CreateTicketTool` | `create_ticket` | Create a ticket in configured provider |
| `GetTicketTool` | `get_ticket` | Fetch ticket details |
| `CloseTicketTool` | `close_ticket` | Close a ticket |
| `ListTicketsTool` | `list_tickets` | List open tickets |
| `AddCommentTool` | `add_comment` | Add a comment to a ticket |

---

## Skill Markdown Format

Skill files are plain Markdown. Optional YAML frontmatter is supported:

```markdown
---
name: tdd-workflow
tags: [testing, tdd, python]
---

# Test-Driven Development

When implementing features, follow this cycle:

1. Write a failing test first.
2. Implement the minimum code to pass the test.
3. Refactor.
```

Skills are discovered from (highest priority first):

1. Project `.claude/skills/` (claude-mpm compatible)
2. `~/.claude/skills/` (user-global claude-mpm)
3. `~/.open-mpm/skills/files/`
4. `config/skills/`

---

## File Extraction Protocol

When an agent produces output containing `## File:` headers, the content is
automatically extracted to `out_dir` at the end of the producing phase. Supported
header formats:

```markdown
## File: path/to/file.py
```python
# file content here
```

### `path/to/file.py`
```python
# backtick-title format also works
```

### File: relative/path.py
```python
# prefixed with ### also works
```
```

The extraction is idempotent — re-running produces identical bytes.
