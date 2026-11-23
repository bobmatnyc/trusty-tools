# /provider and /model Slash Command Design

**Date**: 2026-05-02  
**Scope**: Implementation design for session-level provider and model override commands.

---

## 1. Existing Slash Command Infrastructure

### Parsing and dispatch (`src/repl/mod.rs`)

All slash commands flow through one method on `OpenMpmRepl`:

```
async fn try_handle_slash(&mut self, input: &str) -> Option<Result<(bool, String)>>
```

**Parsing pattern** (lines 308–309):

```rust
let mut parts = input.splitn(2, char::is_whitespace);
let cmd  = parts.next().unwrap_or("");      // "/model"
let arg  = parts.next().map(str::trim).unwrap_or(""); // "claude-haiku-4-5"
```

**Dispatch** is a single `match cmd { ... }` block (lines 313–443). Every arm writes into a `String` buffer (`out`) and returns `Ok(bool)` where `false` means quit. The caller in `ReplBridge::handle_input` sends the buffer as a `ReplEvent::LlmResponse` — no direct stdout/stderr writes allowed inside ratatui mode.

**Special cases handled outside `try_handle_slash`**: `/exit`, `/quit`, and `/clear` are intercepted first in `ReplBridge::handle_input` (lines 1223–1237) because they need to emit additional `ReplEvent`s (scope change, label change) before or instead of the generic `try_handle_slash` path. New commands that mutate visible state should follow the same pattern.

### Help text (`write_help`, line 1110)

The `/help` output is a single literal string. New commands must be added here.

---

## 2. `OpenMpmRepl` Struct Fields (session state)

**File**: `src/repl/mod.rs`, `OpenMpmRepl` struct (lines 42–75).

| Field | Type | Purpose |
|---|---|---|
| `user` | `Option<UserProfile>` | Identity for prompt personalization |
| `project_name` | `String` | Prompt label (e.g. "ctrl", "izzie") |
| `socket_path` | `PathBuf` | UNIX socket for connected project |
| `history_path` | `PathBuf` | On-disk REPL input history |
| `project_dir` | `PathBuf` | Resolved project root (`.open-mpm/` anchor) |
| `agents_dir` | `PathBuf` | `<project_dir>/.open-mpm/agents/` |
| `skills_dir` | `PathBuf` | `<project_dir>/.open-mpm/skills/` |
| `session_id` | `String` | 8-char UUID for this REPL session |
| `git_branch` | `Option<String>` | Current git branch at startup |
| `session_start` | `Instant` | Wall-clock anchor |
| `status_bar` | `StatusBar` | Token counters + model label (crossterm-based, separate from ratatui) |
| `conversation_history` | `Vec<ConversationTurn>` | Multi-turn transcript forwarded to LLM |
| `chat_log` | `Vec<ChatEntry>` | Rendered entries for redraw |
| `active_persona` | `Option<String>` | TOML stem of active persona agent |
| `telegram_handle` | `Option<JoinHandle<()>>` | Background Telegram bot |

**No existing field** for provider override or model override. These would be new fields.

---

## 3. `Ctrl` / Controller State

There is **no `Ctrl` struct** with per-session state. The ctrl layer consists of free functions (`run_pm_task_with_history`, `run_pm_task_with_persona`, `run_pm_task_via_claude_cli`) plus a background actor loop. All LLM routing state is computed fresh per-dispatch from `AgentConfig` + env vars.

**`ConversationTurn`** (`src/ctrl/mod.rs`, line 175): the only cross-turn state passed between the REPL and ctrl — just user/assistant string pairs.

---

## 4. `AgentConfig` and `AgentInfo` Struct Fields

**File**: `src/agents/mod.rs`

### `AgentConfig`

| Field | Type |
|---|---|
| `agent` | `AgentInfo` |
| `llm` | `LlmParams` |
| `system_prompt` | `SystemPrompt` |
| `tools` | `ToolsConfig` |
| `compress` | `AgentCompressConfig` |
| `runner_config` | `RunnerConfig` |
| `adapter` | `Arc<dyn ModelAdapter>` |

### `AgentInfo` (maps to `[agent]` TOML section)

| Field | Type | Notes |
|---|---|---|
| `name` | `String` | TOML stem |
| `role` | `String` | e.g. "controller", "assistant" |
| `model` | `String` | e.g. "claude-sonnet-4-6" |
| `description` | `String` | |
| `runner` | `RunnerKind` | default = `Subprocess` |
| `display_name` | `Option<String>` | Human-friendly name |
| `prompt_label` | `Option<String>` | Short REPL prompt label |
| `persistent_session` | `bool` | |
| `capabilities` | `Option<AgentCapabilities>` | |

### `LlmParams` (maps to `[llm]` TOML section)

| Field | Type | Notes |
|---|---|---|
| `temperature` | `f32` | |
| `max_tokens` | `u32` | |
| `model_override` | `Option<String>` | Beats `[agent].model`, under env var |
| `use_anthropic_direct` | `bool` | Force api.anthropic.com path |
| `enable_prompt_caching` | `bool` | |
| `max_turns` | `u32` | |
| `tool_choice` | `ToolChoice` | |
| `use_finish_task` | `bool` | |
| `claude_allowed_tools` | `Vec<String>` | |
| `aws_profile` | `Option<String>` | |
| `aws_region` | `Option<String>` | |
| `elevation_threshold` | `Option<u32>` | |
| `elevation_model` | `Option<String>` | |

---

## 5. Provider / Runner Routing

### `RunnerKind` enum (`src/agents/mod.rs`, line 300)

```rust
pub enum RunnerKind {
    Subprocess,   // default: re-invoke open-mpm binary as --agent subprocess
    Inline,       // placeholder
    ClaudeCode,   // spawn `claude` CLI via ClaudeCodeAgentRunner
    InProcess,    // run tool loop in-process (no subprocess overhead)
}
```

TOML serialization: `kebab-case` strings (`"subprocess"`, `"claude-code"`, `"in-process"`).

### `LlmCredentials` enum (`src/llm/credentials.rs`, line 29)

```rust
pub enum LlmCredentials {
    ClaudeCode,       // CLAUDE_CODE_OAUTH_TOKEN set → label "claude-code"
    AnthropicDirect,  // ANTHROPIC_API_KEY set      → label "anthropic-direct"
    OpenRouter,       // OPENROUTER_API_KEY set      → label "openrouter"
}
```

Priority order: `ClaudeCode > AnthropicDirect > OpenRouter`.

### Credential-to-runner mapping

| `LlmCredentials` | Behavior | Required credential |
|---|---|---|
| `ClaudeCode` | Short-circuits to `run_pm_task_via_claude_cli` → `ClaudeCodeAgentRunner` | `CLAUDE_CODE_OAUTH_TOKEN` (sk-ant-oat01-*) |
| `AnthropicDirect` | Sets `cfg.llm.use_anthropic_direct = true`; REST path to api.anthropic.com | `ANTHROPIC_API_KEY` (sk-ant-api03-*) |
| `OpenRouter` | Qualifies bare claude model IDs; REST path to openrouter.ai | `OPENROUTER_API_KEY` (sk-or-v1-*) |

Note: `CLAUDE_CODE_OAUTH_TOKEN` is **only** valid for `runner = "claude-code"` agents.  
`ANTHROPIC_API_KEY` is **not** accepted by the OAuth token path — they are mutually exclusive.

### `.open-mpm/agents/ctrl.toml` active fields

```toml
[agent]
model  = "claude-sonnet-4-6"
runner = "claude-code"          # drives ClaudeCodeAgentRunner when CLAUDE_CODE_OAUTH_TOKEN set

[llm]
temperature = 0.5
max_tokens  = 8192
```

---

## 6. Where the Credential Routing Fires and Where to Inject Overrides

### `run_pm_task_with_history` (`src/ctrl/mod.rs`, line 865)

The relevant sequence:

```
1. resolve_agent_config(project_path)   → mut pm_cfg: AgentConfig
2. pick_credentials()                   → creds
3. apply_credential_routing(&mut pm_cfg, &creds)  → bool (short-circuit flag)
   - AnthropicDirect: sets pm_cfg.llm.use_anthropic_direct = true
   - OpenRouter: qualifies pm_cfg.agent.model with "anthropic/" prefix
   - ClaudeCode: returns true → jump to run_pm_task_via_claude_cli
4. [if short-circuit] run_pm_task_via_claude_cli uses pm_cfg.agent.model verbatim
5. [else] build client, run REST loop using pm_cfg.agent.model
```

**Override injection point**: Between steps 1 and 2. After `resolve_agent_config` returns `mut pm_cfg`, apply the session-level overrides:

```rust
if let Some(ref model) = self.model_override {
    pm_cfg.agent.model = model.clone();
}
if let Some(ref runner) = self.runner_override {
    pm_cfg.agent.runner = runner.parse().unwrap_or(pm_cfg.agent.runner);
}
```

However, `run_pm_task_with_history` is a free function that does not receive `&self`. The override must be threaded in as an additional parameter or via the `session_id` argument. **Cleanest approach**: add an `Option<SessionOverrides>` parameter.

### `run_pm_task_with_persona` (`src/ctrl/mod.rs`, line 1249)

Same sequence applies. Override injection after the TOML load at line 1270/1278.

### `attempt_forward` in `OpenMpmRepl` (`src/repl/mod.rs`, line 459)

Both call sites of `run_pm_task_with_persona`:

```rust
crate::ctrl::run_pm_task_with_persona(
    &self.project_dir,
    persona_name,         // or "ctrl"
    task_text,
    &self.conversation_history,
    None,
).await?
```

The REPL holds `&self` at this point, so it has access to whatever override fields are added to `OpenMpmRepl`. An `Option<SessionOverrides>` can be passed as a 6th argument, or the override can be a new `Option<String>` field passed inline.

---

## 7. Where `/model` and `/provider` Overrides Should Live

### Storage location: `OpenMpmRepl` fields

The REPL is the correct home because:
- It is session-scoped (one per REPL process).
- It already owns `active_persona`, `project_name`, and `conversation_history` — all session-level state.
- The overrides survive across turns within a session and are cleared on `/clear` or `/connect`.
- The ctrl free functions (`run_pm_task_with_*`) have no persistent state between calls.

**Proposed new fields**:

```rust
/// Session-level model override set via `/model <name>`.
/// When Some, replaces `config.agent.model` after AgentConfig::load()
/// in every dispatch, before credential routing fires.
model_override: Option<String>,

/// Session-level runner/provider override set via `/provider <name>`.
/// Valid values mirror RunnerKind kebab names and the LlmCredentials labels:
/// "claude-code", "anthropic-direct", "openrouter".
provider_override: Option<String>,
```

### Passing to dispatch functions

Two options, in order of preference:

**Option A — Thin parameter** (least invasive): Add a `model_override: Option<&str>` parameter to the three `run_pm_task_*` functions. `attempt_forward` passes `self.model_override.as_deref()`. The ctrl function applies the override after config load and before credential routing. The provider override can be expressed as a forced `LlmCredentials` variant passed similarly.

**Option B — `SessionOverrides` struct**: Define a small struct:

```rust
pub struct SessionOverrides {
    pub model: Option<String>,
    pub provider: Option<String>,  // "claude-code" | "anthropic-direct" | "openrouter"
}
```

Pass as `Option<&SessionOverrides>` (6th arg). This groups the two fields cleanly and makes future additions (temperature override, etc.) backward-compatible.

Option B is slightly cleaner for future expansion but involves touching more call sites immediately. Option A is simpler for the two fields in scope now.

### Where in the dispatch to apply

In `run_pm_task_with_history` and `run_pm_task_with_persona`, the injection happens **after `AgentConfig::load()`** and **before `apply_credential_routing()`**:

```rust
let (mut pm_cfg, _pm_cfg_path) = resolve_agent_config(project_path).await?;

// --- NEW: apply session-level overrides before credential routing ---
if let Some(ov) = &overrides {
    if let Some(m) = &ov.model {
        pm_cfg.agent.model = m.clone();
    }
    // Provider override: force credential routing to a specific backend
    // by masking what pick_credentials() returns.
}
// --- END ---

let creds = llm::credentials::pick_credentials()...
let claude_cli_short_circuit = apply_credential_routing(&mut pm_cfg, &creds);
```

For the provider override, the cleanest implementation is to let the override replace the `creds` value that would normally come from the env:

```rust
let creds = if let Some(ov) = &overrides && let Some(p) = &ov.provider {
    match p.as_str() {
        "claude-code"       => LlmCredentials::ClaudeCode,
        "anthropic-direct"  => LlmCredentials::AnthropicDirect,
        "openrouter"        => LlmCredentials::OpenRouter,
        _                   => pick_credentials().ok_or_else(...)?,
    }
} else {
    pick_credentials().ok_or_else(...)?
};
```

This means a provider override takes precedence over the ambient credential env vars — which is the intended behavior.

---

## 8. Status Bar and Banner — What Currently Shows, What Must Change

### `StatusBar` (`src/repl/status_bar.rs`)

The `StatusBar` struct has:

```rust
pub struct StatusBar {
    pub model: String,    // set once at construction ("anthropic/claude-sonnet-4-6")
    pub agent: Option<String>,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub session_start: Instant,
    pub config: StatusBarConfig,
}
```

`format_line()` renders:
```
  anthropic/claude-sonnet-4-6 | python-engineer | ↑1234 ↓5678 | 00:01:23
```

`StatusBar.model` is set in `OpenMpmRepl::new()` at line 113:
```rust
let status_bar = StatusBar::new("anthropic/claude-sonnet-4-6", session_start);
```

It is a **hard-coded string** — not derived from the active agent config. When the model override changes, `status_bar.model` must be updated:

```rust
self.status_bar.model = format!("{} ({})", provider_label, model_name);
// or just: self.status_bar.model = model_name;
```

The `status_bar` is rendered via the legacy crossterm path (to stderr), not via ratatui.

### `ReplApp` / TUI (`src/repl/tui.rs`)

`ReplApp` has no model or provider fields. The startup status is passed once as `initial_status: Option<String>` into `run_tui` and stored in `app.status_line`. It is shown as the first line in the chat area with a `[open-mpm]` prefix.

The startup status string is built at REPL start (`src/repl/mod.rs`, lines 159–169):
```rust
let llm_label = crate::llm::credentials::pick_credentials()
    .map(|c| c.label())
    .unwrap_or("none");
let status = format!(
    "✓ LLM: {} ({}) · Tools: {} · Skills: {} · MCP: {} · ...",
    llm_label, model, tool_count, skills_count, mcp_count
);
```

This runs once at startup; it will not auto-update when `/model` or `/provider` is invoked.

### What must change for status visibility

1. **`StatusBar.model`**: Update on every `/model` or `/provider` invocation. Add a method `StatusBar::set_model(String)` or just write `self.status_bar.model` directly.

2. **TUI `status_line`**: The existing `status_line` in `ReplApp` is a one-shot startup field. To show overrides there, emit a `ReplEvent::StatusMessage(...)` from the slash command handler — this appends a `[open-mpm] ...` line to the chat scrollback, which is the appropriate pattern (same as how `/agent` confirms the switch).

3. **No new `ReplEvent` variant needed**: `StatusMessage` and `LabelChanged` already exist and cover the feedback case.

---

## 9. Valid Provider Values and Credential Requirements

| `/provider` arg | Maps to `LlmCredentials` | Required env var | Notes |
|---|---|---|---|
| `claude-code` | `ClaudeCode` | `CLAUDE_CODE_OAUTH_TOKEN` | Routes via `claude` CLI subprocess. Model must be a bare Claude id. |
| `anthropic-direct` | `AnthropicDirect` | `ANTHROPIC_API_KEY` | REST POST to api.anthropic.com. Bare Claude id, no `anthropic/` prefix. |
| `openrouter` | `OpenRouter` | `OPENROUTER_API_KEY` | Bare Claude ids get `anthropic/` prefix injected by `qualify_openrouter_model`. |

`/provider` without an arg should display the current override (or "none, using env default") and list the valid values.

---

## 10. Implementation Checklist

### New `OpenMpmRepl` fields

```rust
model_override: Option<String>,    // set by /model <name>
provider_override: Option<String>, // set by /provider <name>
```

Initialize both to `None` in `OpenMpmRepl::new()`. Clear both in `/clear` and `/connect`.

### New slash command arms in `try_handle_slash`

```rust
"/model" => {
    if arg.is_empty() {
        // show current: override or "none (using agent config)"
    } else if arg == "reset" || arg == "default" {
        self.model_override = None;
        self.status_bar.model = self.resolve_active_model();
        let _ = writeln!(out, "Model override cleared.");
    } else {
        self.model_override = Some(arg.to_string());
        self.status_bar.model = arg.to_string();
        let _ = writeln!(out, "Model override set to: {arg}");
    }
    Ok(true)
}
"/provider" => {
    // similar pattern; validate against known values
    Ok(true)
}
```

### Thread overrides into dispatch

Modify `attempt_forward` to construct a `SessionOverrides` from the REPL fields and pass it to `run_pm_task_with_persona` / `run_pm_task_with_history`.

### Update `write_help`

Add `/model` and `/provider` under a new "Configuration" section.

---

## 11. File Reference Summary

| File | Relevance |
|---|---|
| `src/repl/mod.rs:301–446` | `try_handle_slash` — where new commands go |
| `src/repl/mod.rs:459–538` | `attempt_forward` — where overrides are passed to ctrl |
| `src/repl/mod.rs:42–75` | `OpenMpmRepl` struct — add `model_override`, `provider_override` |
| `src/repl/mod.rs:1110–1139` | `write_help` — update |
| `src/repl/status_bar.rs:53–60` | `StatusBar` struct — `model` field to update |
| `src/repl/tui.rs:87–125` | `ReplApp` — no change needed; use `StatusMessage` event |
| `src/ctrl/mod.rs:865–954` | `run_pm_task_with_history` — inject override after config load |
| `src/ctrl/mod.rs:1249–1303` | `run_pm_task_with_persona` — same injection point |
| `src/ctrl/mod.rs:795–819` | `apply_credential_routing` — reference for override interaction |
| `src/agents/mod.rs:194–266` | `AgentInfo` — `model` and `runner` fields being overridden |
| `src/llm/credentials.rs:29–48` | `LlmCredentials` enum — provider label strings |
| `src/llm/credentials.rs:65–84` | `pick_credentials()` — env-based fallback |
| `.open-mpm/agents/ctrl.toml` | Active agent config showing `runner = "claude-code"`, `model = "claude-sonnet-4-6"` |
