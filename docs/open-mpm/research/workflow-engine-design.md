# Workflow Engine Design — Codebase Analysis

**Date**: 2026-04-22
**Scope**: `src/` full read + TOML configs; informs `src/workflow/` implementation

---

## 1. Entry Point and CLI Flags (`src/main.rs`)

The binary uses manual `args()` scanning (no clap/structopt). Three modes:

| Invocation | Mode | Function |
|---|---|---|
| `open-mpm` (no flags) | PM orchestrator | `run_pm()` |
| `open-mpm --agent <name>` | Sub-agent worker | `run_subagent(name)` |
| `open-mpm --direct <name> [--task-file <path>] [--out-dir <dir>]` | Bypass-PM direct call | `run_direct(name, task_file, out_dir)` |

**How `--direct` works**: reads task from `--task-file` path or stdin, calls `spawn_subagent_and_run(name, task)` directly (same subprocess path PM uses), prints `content` to stdout, optionally runs `extract_files_to_dir()` to materialize `## File: <path>` sections into `--out-dir`.

**How `run_pm` works**: loads `pm.toml`, creates LLM client, reads one line from stdin, calls `llm::chat()` with the `delegate_to_agent` tool, iterates tool calls, for each `delegate_to_agent` call invokes `spawn_subagent_and_run()`, prints result.

**Adding `--workflow`**: insert a new `if let Some(pos) = args.iter().position(|a| a == "--workflow")` block before `run_pm()`, parse the workflow name, call `run_workflow(name).await`.

---

## 2. Agent Loading (`src/agents/mod.rs`)

### Structs

```
AgentConfig         // top-level parsed from TOML
  .agent: AgentInfo
  .llm:   LlmParams
  .system_prompt: SystemPrompt

AgentInfo
  .name:        String
  .role:        String
  .model:       String   // OpenRouter format: "anthropic/claude-sonnet-4-6"
  .description: String

LlmParams
  .temperature: f32
  .max_tokens:  u32

SystemPrompt
  .content: String
```

### Load paths

- `AgentConfig::load(path: &Path) -> Result<Self>` — raw path
- `AgentConfig::by_name(name: &str) -> Result<Self>` — resolves to `config/agents/<name>.toml` relative to CWD

**Important**: `by_name` uses `PathBuf::from("config/agents")` — a relative path from CWD. Sub-agents inherit the parent's CWD when spawned via `Command::new(exe_path)` with no explicit `.current_dir()`, so this works as long as all processes start from the project root. Any workflow engine must ensure the same CWD or use absolute paths.

**Skills injection placeholder**: `SystemPrompt` is a struct (not a bare `String`) specifically to allow "future fields like skill injection paths" per the comment. There is currently no `skills` field — this is the intended extension point.

---

## 3. IPC Protocol (`src/ipc/mod.rs`)

### Enum

```rust
#[serde(tag = "type", rename_all = "snake_case")]
pub enum IpcMessage {
    Task   { id: String, task: String },
    Result { id: String, content: String, status: String },
    Error  { id: String, error: String,   status: String },
}
```

### Wire format (NDJSON — one JSON object per line)

PM → sub-agent (stdin):
```json
{"type":"task","id":"<uuidv4>","task":"<task text>"}
```

Sub-agent → PM (stdout), success:
```json
{"type":"result","id":"<same uuid>","content":"<markdown output>","status":"success"}
```

Sub-agent → PM (stdout), failure:
```json
{"type":"error","id":"<same uuid>","error":"<description>","status":"error"}
```

### Helpers

- `IpcMessage::new_task(task)` — generates fresh UUIDv4 id
- `IpcMessage::new_result(id, content)`
- `IpcMessage::new_error(id, error)`
- `serialize_message(msg) -> Result<String>` — `"{...}\n"` (trailing newline)
- `parse_message(line) -> Result<IpcMessage>` — strips `\r\n`, parses

### Subprocess spawn pattern (from `spawn_subagent_and_run`)

```rust
Command::new(exe_path)
    .args(["--agent", agent_name])
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::inherit())
    .spawn()
```

Two separate `tokio::spawn` tasks (writer + reader) joined with `tokio::join!` to prevent deadlock. Stdin shutdown (EOF) signals to the sub-agent that input is complete.

---

## 4. Tool Dispatch (`src/tools/mod.rs`)

Currently one tool: `delegate_to_agent`. No dispatch table — the PM loop in `main.rs` manually matches `if tc.name != "delegate_to_agent"`.

```rust
// in run_pm():
for tc in response.tool_calls {
    if tc.name != "delegate_to_agent" {
        tracing::warn!(tool = %tc.name, "ignoring unknown tool call");
        continue;
    }
    let agent_name = tc.arguments["agent_name"].as_str()...;
    let task       = tc.arguments["task"].as_str()...;
    spawn_subagent_and_run(&agent_name, &task).await?;
}
```

For new tools (`web_search`, `skill_loader`), you would extend `tools/` with new schema builders and add match arms (or a dispatch map) in the caller context.

---

## 5. LLM Client (`src/llm/mod.rs`)

### Structs

```rust
pub struct ToolCall {
    pub id:        String,
    pub name:      String,
    pub arguments: serde_json::Value,  // pre-parsed from stringified JSON
}

pub struct ChatResponse {
    pub content:    Option<String>,
    pub tool_calls: Vec<ToolCall>,
}
```

### `llm::chat()` signature

```rust
pub async fn chat(
    client:        &Client<OpenAIConfig>,
    model:         &str,
    system_prompt: &str,
    user_message:  &str,
    temperature:   f32,
    max_tokens:    u32,
    tools:         Vec<ChatCompletionTool>,
) -> Result<ChatResponse>
```

Only two messages sent: system + user. No multi-turn conversation history threading. For a workflow phase that needs to pass prior context, you concatenate the previous output into the `user_message` string.

---

## 6. Key Structs/Enums — Complete Reference

| Name | File | Purpose |
|---|---|---|
| `AgentConfig` | `src/agents/mod.rs` | Top-level TOML config |
| `AgentInfo` | `src/agents/mod.rs` | name, role, model, description |
| `LlmParams` | `src/agents/mod.rs` | temperature, max_tokens |
| `SystemPrompt` | `src/agents/mod.rs` | prompt content string |
| `IpcMessage` | `src/ipc/mod.rs` | Task / Result / Error variants |
| `ToolCall` | `src/llm/mod.rs` | Parsed LLM tool invocation |
| `ChatResponse` | `src/llm/mod.rs` | content + tool_calls |

---

## 7. What Needs to Change for the Workflow Engine

### 7a. New CLI flag `--workflow <name>`

Insert in `main.rs` before `run_pm()`:

```rust
if let Some(pos) = args.iter().position(|a| a == "--workflow") {
    let name = args.get(pos + 1)
        .context("--workflow requires a name")?
        .clone();
    return run_workflow(&name).await;
}
```

### 7b. `WorkflowEngine` — `src/workflow/` module layout

```
src/workflow/
├── mod.rs          // pub mod declarations, re-exports
├── config.rs       // WorkflowDef, PhaseDef deserialized from JSON
├── engine.rs       // WorkflowEngine struct, run() method
└── context.rs      // PhaseContext — carries output between phases
```

Register in `src/main.rs`: `mod workflow;`

### 7c. `config/workflows/<name>.json` schema

Suggested structure:

```json
{
  "name": "research-and-implement",
  "description": "Two-phase: research then code",
  "phases": [
    {
      "id": "research",
      "agent": "research-agent",
      "task_template": "Research the following topic: {{user_input}}",
      "tools": ["web_search", "skill_loader"]
    },
    {
      "id": "implement",
      "agent": "python-engineer",
      "task_template": "Implement the following based on this research:\n\n{{research.output}}\n\nTask: {{user_input}}"
    }
  ]
}
```

Corresponding Rust structs in `workflow/config.rs`:

```rust
#[derive(Debug, Deserialize)]
pub struct WorkflowDef {
    pub name:        String,
    pub description: String,
    pub phases:      Vec<PhaseDef>,
}

#[derive(Debug, Deserialize)]
pub struct PhaseDef {
    pub id:            String,
    pub agent:         String,
    pub task_template: String,
    #[serde(default)]
    pub tools:         Vec<String>,
}
```

Load with: `serde_json::from_str::<WorkflowDef>(&raw)`.

### 7d. Context passing between phases

In `workflow/context.rs`:

```rust
pub struct WorkflowContext {
    pub user_input:    String,
    pub phase_outputs: HashMap<String, String>,  // phase id -> content
}
```

Template expansion (simple `{{phase_id.output}}` or `{{user_input}}`):
- Use `str::replace()` for a zero-dependency implementation
- Or add `minijinja = "2"` for full Jinja2 templating (recommended for complex workflows)

### 7e. `engine.rs` run loop sketch

```rust
pub async fn run_workflow(name: &str) -> Result<()> {
    let raw = tokio::fs::read_to_string(
        format!("config/workflows/{name}.json")
    ).await?;
    let def: WorkflowDef = serde_json::from_str(&raw)?;
    let user_input = read_stdin_line().await?;
    let mut ctx = WorkflowContext::new(user_input);

    for phase in &def.phases {
        let task = ctx.render_template(&phase.task_template);
        let result = spawn_subagent_and_run(&phase.agent, &task).await?;
        match result {
            IpcMessage::Result { content, .. } => {
                ctx.phase_outputs.insert(phase.id.clone(), content.clone());
                println!("[{}] {}", phase.id, content);
            }
            IpcMessage::Error { error, .. } => {
                bail!("phase '{}' failed: {}", phase.id, error);
            }
            _ => bail!("unexpected message from phase '{}'", phase.id),
        }
    }
    Ok(())
}
```

`spawn_subagent_and_run` is already in `main.rs` — it needs to be moved to a shared module (e.g., `src/subprocess.rs` or `src/agents/spawn.rs`) so both `run_pm` and `run_workflow` can call it.

### 7f. `web_search` tool

Add `src/tools/web_search.rs`. The tool schema is straightforward. The implementation needs an HTTP client.

**reqwest is NOT in Cargo.toml currently** — add it:

```toml
reqwest = { version = "0.12", features = ["json", "rustls-tls"], default-features = false }
```

For a research agent, the typical flow is: call a search API (Brave Search, SerpAPI, or DuckDuckGo HTML scrape), strip HTML from results, return snippets.

HTML stripping options:
- `scraper = "0.21"` — CSS selector + text extraction (heavier, full HTML parse)
- `ammonia = "4"` — sanitizer/stripper (lighter)
- Manual: `regex` to strip `<[^>]+>` tags (fragile but zero-dep)

Recommended: `scraper` for correctness; it pulls in `html5ever` but handles malformed HTML well.

### 7g. `skill_loader` tool

A tool that reads a named skill file and injects its Markdown content. No new crates needed — just `tokio::fs::read_to_string`. Convention choices:

- Search order: `config/skills/<name>.md` → `~/.claude/skills/<name>/` → `.claude/skills/<name>/`
- Return the file content as the tool result, which the LLM appends to its context

---

## 8. Skill Directory Conventions

**No existing skill-reading code** exists in `src/`. The CLAUDE.md mentions `config/skills/` but the directory is currently empty (it exists but has no files). There is no code that reads from `.claude/skills/` or `~/.claude/skills/`.

The `SystemPrompt` struct is the designated extension point — per comment in `agents/mod.rs`: "kept as a struct to allow future fields like skill injection paths".

Extension approach: add a `skills: Option<Vec<String>>` field to `SystemPrompt` in the TOML schema, read each named file at agent load time, and append to `system_prompt.content`.

---

## 9. Risks and Gotchas

### CWD dependency
`AgentConfig::by_name` uses `PathBuf::from("config/agents")` — a CWD-relative path. Sub-agents are spawned without `.current_dir()`, so they inherit the parent's CWD. This is fine when running from project root but breaks if the binary is invoked from another directory. Mitigation: resolve paths relative to the executable path (`std::env::current_exe().parent()`) or accept an explicit `--config-dir` flag.

### Single-shot sub-agents
Each sub-agent reads exactly one NDJSON line then exits. `run_subagent` does `read_to_string` on all of stdin, then takes `.lines().next()`. Additional lines are silently dropped. The workflow engine sending a second task to the same process will not work — each phase spawn is a fresh process. This is the intended design.

### No tool support in sub-agents
`run_subagent` calls `llm::chat(..., vec![])` — empty tools list. For a research agent that uses `web_search`, you need a new execution mode for sub-agents that supports tool calling. The sub-agent would need its own tool dispatch loop (call LLM → get tool call → execute tool → send result back to LLM → repeat). This is a meaningful implementation delta beyond the current POC.

### Blocking on single LLM response
`llm::chat` has no streaming — waits for the full completion. For a research phase with many web search round trips this will have high latency. Not a correctness risk but affects user experience.

### Template injection via user input
If `task_template` interpolates `{{user_input}}` and user input contains `}}` or newlines, a naive `str::replace` will produce mangled prompts but not a security issue (we control the subprocess). Still worth sanitizing or using a proper template engine.

### `thiserror` in Cargo.toml CLAUDE.md mentions it but it's not in Cargo.toml
CLAUDE.md lists `thiserror` as a dependency but it is absent from `Cargo.toml`. The codebase uses only `anyhow`. If you add typed errors for workflow failures, add `thiserror = "1"` explicitly.

### `tokio-util` and `bytes` are unused
Both crates are in `Cargo.toml` but the NDJSON framing ended up using `tokio::io::BufReader::read_line` directly, not a tokio-util codec. They are dead weight for now — not a risk but worth noting.

---

## 10. External Crates to Add

| Crate | Version | Purpose | Notes |
|---|---|---|---|
| `reqwest` | `0.12` | HTTP for web search | not present; add `features = ["json","rustls-tls"]` |
| `scraper` | `0.21` | HTML parsing/text extraction | brings `html5ever`; needed for search result stripping |
| `minijinja` | `2` | Template engine for task_template | optional; `str::replace` works for simple cases |
| `thiserror` | `1` | Typed error enums for workflow | mentioned in CLAUDE.md but missing from Cargo.toml |
| `serde_json` | already present | Workflow JSON config parsing | already a dependency |

Already present and sufficient: `tokio`, `async-openai`, `serde`, `serde_json`, `anyhow`, `uuid`, `tracing`.

---

## 11. Recommended `src/workflow/` Module Layout

```
src/
├── main.rs                  // add: mod workflow; + --workflow flag dispatch
├── agents/mod.rs            // unchanged; consider adding skills: Option<Vec<String>>
├── ipc/mod.rs               // unchanged
├── llm/mod.rs               // unchanged
├── tools/
│   ├── mod.rs               // add: pub mod web_search; pub mod skill_loader;
│   ├── delegate.rs          // rename current delegate_to_agent_tool fn here
│   ├── web_search.rs        // new: web_search tool schema + HTTP implementation
│   └── skill_loader.rs      // new: skill_loader tool schema + file read implementation
├── subprocess.rs            // new: move spawn_subagent_and_run here (shared by pm + workflow)
└── workflow/
    ├── mod.rs               // pub use engine::run_workflow;
    ├── config.rs            // WorkflowDef, PhaseDef  (serde Deserialize from JSON)
    ├── engine.rs            // WorkflowEngine, run_workflow() async fn
    └── context.rs           // WorkflowContext, render_template()
```

The key structural change is extracting `spawn_subagent_and_run` from `main.rs` into `subprocess.rs` so it can be called from both `run_pm` and `run_workflow` without circular module dependencies.
