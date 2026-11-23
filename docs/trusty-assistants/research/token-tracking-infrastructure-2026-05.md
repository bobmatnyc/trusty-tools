# Token Tracking Infrastructure — Live TUI Display Research

**Date**: 2026-05-02
**Scope**: All LLM dispatch paths, `StatusBar`, `ReplEvent`, and event bus coverage

---

## Executive Summary

The infrastructure for token tracking is substantially complete at the data layer.
`TokenUsage` is captured on all four LLM paths, written to `usage.jsonl`, and
accumulated in `StatusBar`. The gap is that `StatusBar` is never rendered inside
the ratatui TUI — it lives in `src/repl/status_bar.rs` and writes to `stderr` via
crossterm, but the ratatui `draw()` function in `tui.rs` does not reference it.
Additionally, the `attempt_forward` paths all return `TokenUsage::default()` (zero),
so even the accumulator that does exist (`status_bar.add_tokens(...)`) currently
receives zeros for every ctrl/persona/socket-forward turn.

---

## 1. StatusBar Struct and Rendering

**File**: `/Users/masa/Projects/open-mpm/src/repl/status_bar.rs`

```
pub struct StatusBar {
    pub model: String,
    pub agent: Option<String>,
    pub tokens_in: u64,     // cumulative session prompt tokens
    pub tokens_out: u64,    // cumulative session completion tokens
    pub session_start: Instant,
    pub config: StatusBarConfig,  // show_model/agent/tokens/elapsed toggles
}
```

`format_line()` renders: `"  claude-sonnet-4-6 | python-engineer | ↑1234 ↓5678 | 00:01:23  "`

- The `↑` / `↓` indicators ARE wired at the data layer.
- `add_tokens(prompt: u32, completion: u32)` does saturating accumulation.
- `render()` writes to `stderr` via crossterm — NOT via ratatui.
- The ratatui `draw()` function (`tui.rs:634`) has three layout chunks: banner,
  chat, input. There is NO status bar chunk. The `StatusBar` data is in
  `OpenMpmRepl.status_bar` but is never passed to or rendered by the TUI.

**`StatusBar` is not displayed in the current ratatui UI at all.**

---

## 2. Token Update Call Site

**File**: `/Users/masa/Projects/open-mpm/src/repl/mod.rs:243-245`

```rust
let result = self.attempt_forward(task_text).await;
// ...
Ok((response, usage)) => {
    self.status_bar.add_tokens(usage.prompt_tokens, usage.completion_tokens);
    self.status_bar.set_agent(None);
```

This is the only place `status_bar.add_tokens` is called. It fires after every
conversation turn in `forward_task_to_channel`. The `status_bar` object accumulates
tokens correctly — but (a) it never gets non-zero numbers (see section 4), and
(b) the bar is never rendered in the TUI.

---

## 3. Token Data Sources — Per LLM Path

### 3a. OpenRouter path (`src/llm/mod.rs` `chat()`)

Token availability: **Full**

```rust
let usage = response.usage.as_ref().map(|u| {
    let cached = u.prompt_tokens_details.as_ref()
        .and_then(|d| d.cached_tokens).unwrap_or(0);
    TokenUsage::new(u.prompt_tokens, u.completion_tokens, cached, 0)
}).unwrap_or_default();
```

- `prompt_tokens`, `completion_tokens`, `cache_read_tokens` all extracted.
- `emit_llm_responded(model, started, Some(usage.completion_tokens))` publishes
  `Event::LlmResponded` carrying `completion_tokens`.
- `record_dispatch_usage(...)` writes to `usage.jsonl`.
- Returns `TokenUsage` on every `ChatResponse`.

### 3b. Anthropic-direct path (`src/llm/anthropic_native.rs` / `chat_with_tools_gated`)

Token availability: **Full**

From `llm/mod.rs:897`:
```rust
TokenUsage::new(u.prompt_tokens, u.completion_tokens, cached, 0)
```
Same extraction as OpenRouter. `emit_llm_responded` fires at line 919.
`total_usage.add(&turn_usage)` accumulates across tool-loop turns.

### 3c. Claude CLI path (`src/agents/claude_code_runner.rs` `run_with_config_ctx`)

Token availability: **Full (from stream-json `result` event)**

```rust
if let Some(u) = event.get("usage") {
    result_usage = TokenUsage {
        prompt_tokens: u.get("input_tokens")...,
        completion_tokens: u.get("output_tokens")...,
        cache_read_tokens: u.get("cache_read_input_tokens")...,
        cache_creation_tokens: u.get("cache_creation_input_tokens")...,
    };
}
```

- Populated from the terminal `{"type":"result","usage":{...}}` event.
- Zero when older `claude` CLI versions omit the `usage` block.
- Written to `usage.jsonl` via `UsageRecord`.
- Returned as `AgentOutput.usage` to callers.

### 3d. Bedrock path (`src/llm/bedrock.rs` via `chat_adapter_aware`)

Token availability: **Full**

`emit_llm_responded(model, started, Some(usage.completion_tokens))` at
`mod.rs:642`. `record_dispatch_usage` at line 644.

---

## 4. The Token Gap: `attempt_forward` Returns Zero

**File**: `/Users/masa/Projects/open-mpm/src/repl/mod.rs:481-569`

Every branch of `attempt_forward` returns `TokenUsage::default()` (all zeros):

```rust
// persona path
return Ok((response, TokenUsage::default()));   // line 498

// socket User scope
Ok((response, TokenUsage::default()))            // line 525

// socket Project scope
Ok((response, TokenUsage::default()))            // line 535

// no-socket User scope
Ok((response, TokenUsage::default()))            // line 567
```

`run_pm_task_with_persona` and `run_pm_task_with_history` return `String`, not
`(String, TokenUsage)`. Token data never makes it back to the REPL layer.

`forward_task_to_channel` does call `status_bar.add_tokens(usage.prompt_tokens,
usage.completion_tokens)` but these are always 0,0.

---

## 5. Event Bus — What Exists vs. What's Needed

### Existing events with token data

`Event::LlmResponded` (in `src/events.rs:167`):
```rust
LlmResponded {
    session_id: String,
    agent_name: String,
    model: String,
    completion_tokens: Option<u32>,
    latency_ms: u64,
}
```

This fires from `emit_llm_responded` inside `llm/mod.rs` on every OpenRouter,
Anthropic-direct, and Bedrock call. It carries only `completion_tokens`, not
`prompt_tokens`.

`Event::LlmRequested` carries `prompt_tokens: Option<u32>`, currently always
`None` (`emit_llm_requested(model, None)` — not populated before the call).

### Missing

- No `Event::TokensUsed` variant that carries both prompt + completion as a
  paired update for the status bar.
- No variant that carries `TokenUsage` (the 4-field struct with cache tokens).
- The `spawn_thinking_relay` in `mod.rs:1164` listens to the event bus but does
  NOT handle `LlmResponded` — token events are invisible to the REPL layer.
- There is no `ReplEvent::TokenUpdate` variant in `tui.rs`.

---

## 6. ReplEvent Enum — Current State

**File**: `/Users/masa/Projects/open-mpm/src/repl/tui.rs:134-159`

```rust
pub enum ReplEvent {
    LlmResponse { text: String, is_error: bool },
    LlmThinking(bool),
    ThinkingStep(String),
    StatusMessage(String),
    LabelChanged(String),
    AgentScopeChanged(AgentScope),
    Key(KeyEvent),
    Resize(u16, u16),
    Submit(String),
}
```

No token update variant exists. Token counts would need a new variant, e.g.:
`TokenUpdate { prompt: u32, completion: u32 }`.

---

## 7. ReplApp State — What Needs Adding

`ReplApp` in `tui.rs:87` has no token fields. The status bar data lives only in
`OpenMpmRepl.status_bar`, which is inside the `Arc<Mutex<OpenMpmRepl>>` — it is
NOT copied into `ReplApp` which is the state driven by the ratatui renderer.

To display tokens in the TUI, `ReplApp` needs:
```rust
pub tokens_in: u64,
pub tokens_out: u64,
pub model: String,
```

---

## 8. Recommended Implementation Approach

### Minimal path (4 changes)

**Step 1: Add `TokenUpdate` to `ReplEvent` and `tokens_in`/`tokens_out` to `ReplApp`**

In `tui.rs`, add:
```rust
// ReplEvent
TokenUpdate { prompt: u32, completion: u32 },

// ReplApp
pub tokens_in: u64,
pub tokens_out: u64,
```

**Step 2: Handle `TokenUpdate` in `process_event`**

```rust
ReplEvent::TokenUpdate { prompt, completion } => {
    let mut a = app.lock().await;
    a.tokens_in = a.tokens_in.saturating_add(prompt as u64);
    a.tokens_out = a.tokens_out.saturating_add(completion as u64);
}
```

**Step 3: Emit `TokenUpdate` from the thinking relay OR from `forward_task_to_channel`**

Option A (event bus relay): Extend `spawn_thinking_relay` to also handle
`Event::LlmResponded` and emit `ReplEvent::TokenUpdate`. This works for all paths
that publish `LlmResponded` (OpenRouter, Anthropic-direct, Bedrock, and claudecode
via `AgentOutput.usage` — though the claude-code path doesn't publish
`LlmResponded` directly, it returns `AgentOutput.usage`).

Option B (return usage through `attempt_forward`): Change `run_pm_task_with_persona`
and `run_pm_task_with_history` to return `(String, TokenUsage)`, propagate from
the underlying LLM calls, and populate the existing `status_bar.add_tokens` call.
Then add `ReplEvent::TokenUpdate` emit after the `Ok((response, usage))` branch.

Option A is lower-risk for the ctrl/persona path (pure event interception, no
signature changes). Option B is more precise (captures per-turn totals) but
requires threading `TokenUsage` through `ctrl::run_pm_task_*` signatures.

**Step 4: Render token counts in `draw_input` or add a status bar row to `draw()`**

Cheapest: add to `draw_input` right side (mirrors existing `[thinking...]`
indicator):
```rust
let token_label = format!("↑{} ↓{}", app.tokens_in, app.tokens_out);
// render in dim style on the right side of the input bar
```

Or add a fourth layout chunk of `Constraint::Length(1)` for a dedicated status row.

### Granularity

- Per-turn tokens: both prompt + completion reported each turn from `TokenUpdate`.
- Cumulative session: `tokens_in`/`tokens_out` in `ReplApp` accumulate over all turns.
- Both are available with the approach above.

---

## 9. What's Already Wired (Summary Table)

| Path | Token data available | Written to usage.jsonl | StatusBar.add_tokens | TUI display |
|---|---|---|---|---|
| OpenRouter (`chat()`) | Yes — full `TokenUsage` | Yes | No (zero via `attempt_forward`) | No |
| Anthropic-direct | Yes — full `TokenUsage` | Yes | No (zero via `attempt_forward`) | No |
| Claude CLI (`claude_code_runner`) | Yes — from stream-json `result` | Yes | No (zero via `attempt_forward`) | No |
| Bedrock | Yes — full `TokenUsage` | Yes | No (zero via `attempt_forward`) | No |
| StatusBar data model | Full (tokens_in/out fields) | N/A | Wired, receives zeros | No (crossterm, not ratatui) |
| ReplEvent bus | No `TokenUpdate` variant | N/A | N/A | N/A |

---

## 10. Files to Change for Implementation

| File | Change |
|---|---|
| `src/repl/tui.rs` | Add `TokenUpdate { prompt, completion }` to `ReplEvent`; add `tokens_in`, `tokens_out` to `ReplApp`; handle in `process_event`; render in `draw_input` or new status row |
| `src/repl/mod.rs` | Extend `spawn_thinking_relay` to relay `Event::LlmResponded` as `ReplEvent::TokenUpdate`; or propagate `TokenUsage` through `attempt_forward` |
| `src/ctrl/mod.rs` | (Option B only) Change `run_pm_task_with_persona` / `run_pm_task_with_history` return type to `(String, TokenUsage)` |
| `src/events.rs` | (Optional) Add `prompt_tokens` to `LlmResponded`, or add dedicated `TokensConsumed` event |
