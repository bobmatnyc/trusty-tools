# LLM Dispatch & Response Pathway Analysis

**Date**: 2026-05-01
**Scope**: All LLM call entry points, credential routing, and response delivery paths

---

## 1. Pathway Map — All LLM Call Entry Points

| # | Function | File:Line | Credential Path | Unified / Standalone |
|---|----------|-----------|-----------------|----------------------|
| 1 | `ctrl_chat_turn` | `src/ctrl/mod.rs:3391` | Hard-wired `CTRL_MODEL` ("anthropic/claude-sonnet-4-6"), `llm::chat()` via OpenRouter only. **No `pick_credentials` call**. | Standalone — bypasses credential router |
| 2 | `run_pm_task_with_history` (REST fast-path) | `src/ctrl/mod.rs:961` | `pick_credentials()` → AnthropicDirect or OpenRouter → `llm::chat_with_tools_gated()` | Unified via `pick_credentials` |
| 3 | `run_pm_task_with_history` (REST tool-armed) | `src/ctrl/mod.rs:1053` | Same as #2 | Unified via `pick_credentials` |
| 4 | `run_pm_task_with_history` (ClaudeCode branch) | `src/ctrl/mod.rs:800` | ClaudeCode → `run_pm_task_via_claude_cli` → `ClaudeCodeAgentRunner::run_with_config_public` | Unified via `pick_credentials` |
| 5 | `run_pm_task_with_persona` (REST path) | `src/ctrl/mod.rs:1323` | `pick_credentials()` → AnthropicDirect or OpenRouter → `llm::chat_with_tools_gated()` | Unified via `pick_credentials` |
| 6 | `run_pm_task_with_persona` (ClaudeCode branch) | `src/ctrl/mod.rs:1179` | ClaudeCode → `run_pm_task_via_claude_cli` | Unified via `pick_credentials` |
| 7 | `InProcessAgentRunner::run_inner` | `src/agents/in_process_runner.rs:295` | `use_anthropic_direct` flag from agent TOML; does **not** call `pick_credentials()` — relies on caller to set the flag | Standalone — no auto-routing |
| 8 | `chat_adapter_aware` (Bedrock) | `src/llm/mod.rs:502` | AWS Bedrock credentials via `OPEN_MPM_AWS_PROFILE`/`OPEN_MPM_AWS_REGION` env vars | Standalone — Bedrock-specific |
| 9 | `llm::chat_with_tools_gated` (Bedrock branch) | `src/llm/mod.rs:648` | Bedrock via env vars | Standalone — Bedrock-specific |
| 10 | `llm::chat_with_tools_gated` (Anthropic native) | `src/llm/mod.rs:735` | `use_anthropic_direct` flag → `send_anthropic_native_completion` | Unified — routes via flag |
| 11 | `llm::chat_with_tools_gated` (OpenRouter raw) | `src/llm/mod.rs:767` | `send_raw_completion` via OpenRouter | Unified — routes via flag |
| 12 | `llm::chat_with_tools_gated` (async-openai typed) | `src/llm/mod.rs:782` | OpenRouter via `async-openai` client | Unified |
| 13 | Telegram bot handler | `src/telegram/mod.rs:311` | Delegates to `run_pm_task_with_history` → same as #2-4 | Unified via `run_pm_task_with_history` |

### Key observation

`ctrl_chat_turn` (entry point #1) is the **legacy stdin REPL path** (`run_ctrl_inner(with_stdin=true)`) and calls `llm::chat()` with a hardcoded model constant and no credential routing. It is only reached from the stdin loop at `src/ctrl/mod.rs:2179`. The ratatui REPL path goes through `run_pm_task_with_history` instead.

---

## 2. Response Delivery Paths

### Path A — Ratatui REPL (primary interactive path)

```
User keypress (Enter)
  → repl/tui.rs: TUI event loop
  → repl/mod.rs:handle_input()          [spawns tokio::spawn]
    → repl/mod.rs:forward_task_to_channel(task, tx)
      → repl/mod.rs:attempt_forward(task)
          [if persona active]   → ctrl::run_pm_task_with_persona()
          [if socket alive]     → ctrl::forward_to_controller(stream) [Unix socket IPC]
          [else, in-process]    → ctrl::run_pm_task_with_history()
              → pick_credentials()
              [ClaudeCode]      → run_pm_task_via_claude_cli → ClaudeCodeAgentRunner
              [Anthropic/OR]    → llm::chat_with_tools_gated()
                                     → send_anthropic_native_completion OR
                                       send_raw_completion OR
                                       client.chat().create()
      → tx.send(ReplEvent::LlmResponse { text, usage })
  → repl/tui.rs: renders ReplEvent::LlmResponse to terminal
```

### Path B — PM orchestrator (sub-agent delegation)

```
ctrl::run_pm_task_with_history()
  → llm::chat_with_tools_gated()
  → model returns tool call: delegate_to_agent(agent_name, task)
  → DelegateToAgentTool::execute()
    → DispatchingAgentRunner::run(agent_name, task)
        [RunnerKind::ClaudeCode]    → ClaudeCodeAgentRunner::run()
                                         → claude CLI subprocess via tokio::process
        [RunnerKind::InProcess]     → InProcessAgentRunner::run_inner()
                                         → llm::chat_with_tools_gated()
        [RunnerKind::Subprocess]    → SubprocessAgentRunner (NDJSON IPC)
  → result string returned up call chain → back to chat_with_tools_gated tool loop
  → final text → run_pm_task_with_history → attempt_forward → ReplEvent::LlmResponse
```

### Path C — Persona path (/agent izzie)

```
User: "/agent izzie"
  → repl/mod.rs:try_handle_slash()
    → sets repl.active_persona = Some("izzie")
    → returns status message via ReplEvent::LlmResponse

User: next message
  → handle_input() → forward_task_to_channel()
    → attempt_forward() detects active_persona
    → ctrl::run_pm_task_with_persona("izzie", input, history)
        → pick_credentials()
        [ClaudeCode]  → run_pm_task_via_claude_cli (persona_cfg)
        [Other]       → llm::chat_with_tools_gated (persona tool registry)
    → result → ReplEvent::LlmResponse
```

### Path D — Telegram

```
Telegram user message
  → src/telegram/mod.rs: teloxide handler
    → loads conversation history from session store
    → ctrl::run_pm_task_with_history(&path, &text, &history, None)
        [identical to Path A's in-process branch]
    → result text → bot.send_message(chat_id, text)
```

---

## 3. Duplication Hotspots

### 3a. `pick_credentials()` + model qualification copied twice

```
src/ctrl/mod.rs:738-806   (run_pm_task_with_history)
src/ctrl/mod.rs:1173-1186 (run_pm_task_with_persona)
```

Both functions contain an identical 3-way credential block:
1. `pick_credentials()` → bail if None
2. `if AnthropicDirect { cfg.llm.use_anthropic_direct = true; }`
3. `if ClaudeCode { return run_pm_task_via_claude_cli(...) }`
4. `qualify_openrouter_model(&creds, &cfg.agent.model)`

### 3b. Deployment context block built twice

```
src/ctrl/mod.rs:768-793   (run_pm_task_with_history injects into pm_cfg.system_prompt)
src/ctrl/mod.rs:3538-3580 (ctrl_chat_turn builds same block into system_prompt string)
```

The `## Deployment Configuration` footer is assembled with different field sets but the same purpose and structure.

### 3c. `llm::create_client()` called at every chat turn

```
src/ctrl/mod.rs:818   (run_pm_task_with_history)
src/ctrl/mod.rs:1188  (run_pm_task_with_persona)
src/ctrl/mod.rs:3392  (ctrl_chat_turn)
src/agents/in_process_runner.rs:~130 (InProcessAgentRunner::new, but stored on struct)
```

`run_pm_task_with_history` and `run_pm_task_with_persona` allocate a fresh `reqwest`-backed client per request rather than reusing a process-level client. `InProcessAgentRunner` correctly builds the client once at construction time.

### 3d. `ctrl_chat_turn` bypasses credential routing entirely

```
src/ctrl/mod.rs:3583-3592
```

`ctrl_chat_turn` calls `llm::chat()` with `CTRL_MODEL` (hardcoded `"anthropic/claude-sonnet-4-6"`) and a fresh client, with no `pick_credentials()` call. This means the legacy stdin REPL always routes via OpenRouter regardless of which credential is configured, and ignores `ANTHROPIC_API_KEY` and `CLAUDE_CODE_OAUTH_TOKEN`.

### 3e. `println!`/`eprintln!` in `run_ctrl_inner` (stdin loop only)

```
src/ctrl/mod.rs:1747, 1770, 1771, 1775, 1779, 1784-1785, 1787, 1793-1799, 1813-1814, 1817, 1828-1840, 1847, 1851, 1893
```

All `println!`/`eprintln!` calls are confined to `handle_command` (the legacy stdin REPL at line 1761) and `run_ctrl_inner`. They are not reachable from the ratatui path. No TTY leak in async agent paths.

---

## 4. Top Consolidation Recommendations (ranked by impact)

### 1. Extract `apply_credential_routing(cfg, creds)` helper — HIGH impact

**Problem**: The 3-way credential block is copy-pasted at `ctrl/mod.rs:738-806` and `ctrl/mod.rs:1173-1186`. Any future credential type (e.g. GCP Vertex) requires a change in both places.

**Fix**: Extract a free function:
```rust
fn apply_credential_routing(
    cfg: &mut AgentConfig,
    creds: &LlmCredentials,
) -> bool /* returns true if ClaudeCode short-circuit needed */
```
Both `run_pm_task_with_history` and `run_pm_task_with_persona` call it before dispatching. Eliminates ~30 lines of duplication and makes credential precedence a single source of truth.

---

### 2. Fix `ctrl_chat_turn` credential blindness — HIGH impact (correctness bug)

**Problem**: `ctrl_chat_turn` (`ctrl/mod.rs:3391`) hardcodes `CTRL_MODEL` and calls `llm::chat()` directly. Users who configure `ANTHROPIC_API_KEY` or `CLAUDE_CODE_OAUTH_TOKEN` still hit OpenRouter from the legacy stdin REPL path.

**Fix**: Insert `pick_credentials()` at the top of `ctrl_chat_turn`, apply the same routing logic (or call the helper from rec #1), and replace `llm::chat()` with `llm::chat_with_tools_gated()` so it participates in the unified dispatch. Also replaces the hardcoded `1024` max-tokens limit.

---

### 3. Promote `llm::create_client()` to a process-level singleton — MEDIUM impact (performance)

**Problem**: `run_pm_task_with_history` and `run_pm_task_with_persona` each call `llm::create_client()` per request, allocating a new `reqwest::Client` (connection pool + TLS state) every turn.

**Fix**: Mirror the `HTTP_CLIENT: OnceLock<reqwest::Client>` pattern already present in `llm/mod.rs:196` — add `OPENAI_CLIENT: OnceLock<Client<OpenAIConfig>>` initialized on first call. Both ctrl functions then take `&'static Client<OpenAIConfig>` without heap allocation per turn.

---

### 4. Unify the `## Deployment Configuration` footer injection — LOW-MEDIUM impact

**Problem**: `run_pm_task_with_history` (line 779) and `ctrl_chat_turn` (line 3562) both append a deployment context block with overlapping but slightly different fields. The block in `ctrl_chat_turn` has `runner`, `tools_count`, `mcp_count`, `config_label` that the other lacks; neither block is reused.

**Fix**: Create `build_deployment_footer(model, runner_label, version, skills, tools, mcp, project, config) -> String` in a shared location (e.g. `agents/prompt_builder.rs` or `ctrl/mod.rs`). Both call sites pass their available fields.

---

### 5. Thread `TokenUsage` out of `attempt_forward` for persona and socket paths — LOW impact (observability)

**Problem**: `attempt_forward` returns `(String, TokenUsage)` but both the persona branch (line 451) and socket-forward branch (line 463) return `TokenUsage::default()`, so the token counter in the status bar is always zero for those paths.

**Fix**: `run_pm_task_with_persona` and `ctrl::forward_to_controller` should return `(String, TokenUsage)` and propagate usage from `chat_with_tools_gated`. The socket path is harder (IPC), but the persona path is straightforward.
