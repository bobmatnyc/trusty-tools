# TM Promptable Architecture Research
**Date**: 2026-05-04  
**Purpose**: Everything needed to implement "fully promptable TM" — LLM-driven tmux session management via ctrl/PM tools.

---

## 1. How to Add a Tool (exact pattern)

Implement the `ToolExecutor` trait (`src/tools/traits.rs`):

```rust
#[async_trait]
impl ToolExecutor for MyTmTool {
    fn name(&self) -> &str { "tm_list_sessions" }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "tm_list_sessions",
                "description": "...",
                "parameters": {
                    "type": "object",
                    "properties": { /* args */ },
                    "required": [],
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        // ... call self.tm_manager.list_sessions().await
        ToolResult::ok("json result")
        // or: ToolResult::err("reason")
    }
}
```

Then register with:
```rust
registry.register(Arc::new(MyTmTool { tm: self.tm_manager.clone() }));
```

`debug_assert!` fires on duplicate names in debug builds — names must be unique.

---

## 2. Where Tools Are Registered

**Two separate registries**, built per-turn (not global):

### `ctrl_chat_turn` registry — `build_ctrl_registry()` at line 3709
`src/ctrl/mod.rs:3913` calls `build_ctrl_registry(...)` which builds and returns a fresh `ToolRegistry` each turn. Current tools: `start_pm`, `search_sessions`, `list_projects`, `memory_store/recall`, `task_status`, `self_project_status`, `initiate_self_task`, `search_docs`, `add/remove_project`, `stop_task`, `set_active_project`, `move_file`, `create_dir`, `brave_search`, `search_code`, `mcp_*` tools, ticketing tools, git tools.

**To add TM tools to ctrl**: add `registry.register(Arc::new(TmListSessionsTool { ... }))` inside `build_ctrl_registry()`. The `TmManager` Arc must be passed in.

### `run_pm_task_with_history` registry — at line 1283
Separate inline registry. Tools: `delegate_to_agent`, `add/list/remove_project`, `stop_task`, `set_active_project`, `move_file`, `create_dir`, `brave_search`, `search_code`, `run_bash`, `mcp_*`, ticketing. The PM does NOT currently have TM tools.

**Tools are per-call, not per-agent.** No per-agent allowlist is enforced at `ctrl_chat_turn` level (allowlist gating is optional via `dispatch_gated`).

---

## 3. System Prompt Assembly

### `ctrl_chat_turn` (src/ctrl/mod.rs ~3956)
1. Load `ctrl.toml` → `base_prompt` (or fall back to `CTRL_SYSTEM_PROMPT` constant)
2. `SystemPromptBuilder::new(base_prompt)`
3. `.add_skill(text)` for each skill in `[system_prompt] skills = [...]`
4. `.add_mcp_layer(section)` from MCP config
5. If not `is_ctrl_persona`: `.add_memory_layer(memories)` from project memory recall
6. `.build()` → final string
7. Post-build appends (string push): self-project footer, user profile block, current datetime, deployment footer

**To inject TM context**: append a TM status block after step 6, e.g.:
```rust
system_prompt.push_str(&format!("\n\n## Active TM Sessions\n{}", tm_summary));
```
Or pass it as a layer via `builder.add_skill(tm_context_block)`.

### `run_pm_task_with_history` (src/ctrl/mod.rs ~1139)
1. `build_user_context_prefix(&pm_cfg.system_prompt.content)` → datetime-prefixed base
2. `SystemPromptBuilder::new(base)` + `.add_mcp_layer()` + `.add_memory_layer()`
3. `.build()`
4. `filter_project_index_in_prompt()` applied to base (relevance filters `## Project Context (auto-indexed)` section)
5. Deployment footer appended

---

## 4. Project Index Injection

The `## Project Context (auto-indexed)` section is embedded in the agent TOML's `[system_prompt] content`. At runtime, `filter_project_index_in_prompt(system_prompt, task, top_n=15)` at line 1713 locates this header, extracts the body, runs `context_filter::filter_index_entries(body, task, top_n)` (TF-IDF relevance), and replaces the body with only the top-N entries. Applied only in `run_pm_task_with_history`.

---

## 5. TmManager API Surface (src/tm/manager.rs)

```rust
impl TmManager {
    pub fn new(state_dir: &Path) -> Result<Self>
    pub async fn new_session(name, project_path, adapter_type) -> Result<TmSession>
    pub fn attach_instructions(name_or_id) -> Result<String>
    pub async fn pause_session(name_or_id) -> Result<()>
    pub async fn resume_session(name_or_id) -> Result<()>
    pub async fn kill_session(name_or_id) -> Result<()>
    pub async fn send_message(name_or_id, message) -> Result<()>
    pub async fn capture_pane(name_or_id, lines: u32) -> Result<String>
    pub async fn list_sessions() -> Result<Vec<TmSession>>
    pub async fn list_projects() -> Result<Vec<TmProject>>
    pub async fn detect_adapter(name_or_id) -> Result<(AdapterType, f32)>
    pub async fn get_or_create_project(path) -> Result<TmProject>
    pub async fn reconcile() -> Result<ReconcileReport>
    pub async fn poll_sessions(...) -> ...
}
```

---

## 6. Tool Call → Execution → Result Data Flow

```
LLM response contains tool_call { name: "tm_list_sessions", arguments: "{...}" }
    │
    ▼
llm::chat_with_tools_gated loop detects tool_call
    │
    ▼
registry_arc.dispatch_gated(name, args, allowed=None)  // or dispatch()
    │
    ▼
TmListSessionsTool::execute(args: Value) -> ToolResult
    │ calls self.tm.list_sessions().await
    ▼
ToolResult::ok(json_string)  // or ToolResult::err(reason)
    │
    ▼
Serialized back as ChatCompletionRequestToolMessageArgs with is_error flag
    │
    ▼
Appended to messages vec, loop continues for next LLM turn
    │
    ▼
LLM synthesizes final text response from tool results
```

---

## 7. ctrl.toml and pm.toml System Prompts (first 30 lines)

**ctrl.toml**: "You are ctrl — the coordination layer for open-mpm. You sit between the user and the PM orchestrator. Two modes: Assistant (direct chat) and Coordinator (delegates via PM). Triage logic: simple questions → direct; status → tools; code/research/QA/docs → delegate to PM; ambiguous → clarify; risky → confirm."

**pm.toml**: "You are the PM orchestrator for open-mpm. Analyze task → select agent → delegate. Agent selection: README → docs-agent; Python → python-engineer; Bash/Docker → local-ops-agent; multi-file → plan-agent; code questions → research-agent (read-only); QA → qa-agent. Template var `{{available_agents}}` is filled at runtime."

---

## 8. Implementation Plan for Fully Promptable TM

1. **Create `src/tools/tm_tools.rs`** — one `ToolExecutor` per TM operation: `tm_list_sessions`, `tm_new_session`, `tm_send_message`, `tm_capture_pane`, `tm_pause_session`, `tm_resume_session`, `tm_kill_session`. Each holds `Arc<TmManager>`.

2. **Wire into `build_ctrl_registry()`** — add `Arc<TmManager>` as a new parameter, register TM tools inside the function body.

3. **Inject TM context into ctrl system prompt** — after `builder.build()`, append a live TM session summary (e.g. from `tm.list_sessions()`) so the LLM knows current session state without needing to call `tm_list_sessions` first.

4. **Update `ctrl_chat_turn` call site** — pass the `TmManager` Arc from `Ctrl` struct into `build_ctrl_registry`.

5. **Add `tm_manager` field to `Ctrl` struct** — optional `Option<Arc<TmManager>>` to support environments without tmux.
