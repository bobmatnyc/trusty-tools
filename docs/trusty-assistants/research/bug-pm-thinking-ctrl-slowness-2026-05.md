# Bug Analysis: "PM thinking…" in ctrl path + Slowness (2026-05)

## Summary

Two related issues in the ctrl direct-chat path (AgentScope::User):

1. `⟳ PM thinking…` still appears in TUI despite PR #283 claiming to remove it.
2. Requests are unacceptably slow.

---

## Issue 1: "PM thinking…" still showing on ctrl/direct-chat path

### Root cause

PR #283 removed `Event::PmThinking` emissions from `run_pm_task_with_persona` and
`run_pm_task_via_claude_cli`, but **`run_pm_task_with_history` was not changed**. The
`PmThinking` emission at line 884 of `src/ctrl/mod.rs` is the FIRST statement in that
function and fires unconditionally.

The bug: when `attempt_forward` in `src/repl/mod.rs` (line 487–507) is called with
`AgentScope::User` and the socket probe fails, it calls:

```
run_pm_task_with_persona("ctrl", …)
```

That function short-circuits to `run_pm_task_via_claude_cli` when
`CLAUDE_CODE_OAUTH_TOKEN` is active. The CLI path itself does NOT emit `PmThinking`.
So far so good — PR #283 should have fixed this.

**BUT:** The observed symptom sequence is:

```
⟳ thinking…           ← default empty thinking_lines text (TUI default, no event)
⟳ PM thinking…        ← PmThinking event fired
⟳ pm · running (claude-code)…  ← AgentStarted event fired
```

This means `PmThinking` IS still being published. The remaining emission sites are:

| File | Line | Function | Condition |
|---|---|---|---|
| `src/ctrl/mod.rs` | 884 | `run_pm_task_with_history` | Always, at entry |
| `src/ctrl/mod.rs` | 1071 | `run_pm_task_with_history` | Name greeting fast-path |
| `src/ctrl/mod.rs` | 1112 | `run_pm_task_with_history` | Conversational fast-path result |
| `src/ctrl/mod.rs` | 1205 | `run_pm_task_with_history` | Tool-armed delegation result |

### Why PR #283 didn't fully fix it

The fix in #283 correctly removed emissions from `run_pm_task_with_persona` and
`run_pm_task_via_claude_cli`. However, it missed the case where `run_pm_task_with_history`
is still called.

The likely remaining call path that triggers the symptom is:

**Hypothesis A: socket path is alive (CtrlSocket::probe_default succeeds)**

When a controller socket IS alive, `attempt_forward` calls `ctrl::forward_to_controller`,
which sends the message over the socket to the running controller process. The running
controller handles it via `handle_socket_connection` → `run_pm_task_with_history`
(line 1660 in ctrl/mod.rs). That function emits `PmThinking` at line 884 as its first
action. This runs in the SAME process, so the event bus is shared, and the `ThinkingStep`
relay in `spawn_thinking_relay` catches it.

**Hypothesis B: scope detection is wrong**

If `current_scope()` returns `AgentScope::Project` instead of `AgentScope::User` for
the direct ctrl chat, then `attempt_forward` takes the `AgentScope::Project` branch
and calls `run_pm_task_with_history` directly (line 499).

The `AgentStarted` event shows `pm · running (claude-code)`, where the agent name is
`pm`. This is the agent name from the pm.toml config, NOT `ctrl`. This is a strong
signal that `run_pm_task_with_history` is being invoked (which loads pm.toml via
`resolve_agent_config`), NOT `run_pm_task_with_persona("ctrl", …)`.

### Definitive conclusion

The observed `⟳ PM thinking…` followed by `⟳ pm · running (claude-code)…` (where the
agent name is `pm`, not `ctrl`) proves that **`run_pm_task_with_history` is executing**,
not `run_pm_task_with_persona`. This means either:

1. The socket probe succeeds and the request routes to `handle_socket_connection` which
   calls `run_pm_task_with_history`; OR
2. `current_scope()` returns `AgentScope::Project` unexpectedly, bypassing the
   `run_pm_task_with_persona` branch.

The `AgentStarted` event with `agent_name = "pm"` (from pm.toml) vs. `agent_name = "ctrl"`
(what ctrl.toml would produce) is the distinguishing evidence. If ctrl.toml were being
loaded, the agent name would be `ctrl`, not `pm`.

### Fix

**Primary fix** (applies regardless of which hypothesis is correct):

Remove the `Event::PmThinking` emission from `run_pm_task_with_history` entirely, or
gate it behind a scope/mode flag. The function should not announce "PM thinking" when it
is being called in a context that is NOT the PM orchestrator (e.g., socket handler,
direct API).

**Secondary fix** (the real architectural issue):

The socket handler at `src/ctrl/mod.rs:1660` always calls `run_pm_task_with_history`.
If a ctrl-mode session is connected and the user sends a message, the socket path
routes through the PM orchestrator instead of the ctrl persona. The fix is for
`handle_socket_connection` to check the request's scope/persona and route accordingly.

**Tertiary fix** (scope detection in repl):

Verify `current_scope()` in `src/repl/mod.rs` correctly returns `AgentScope::User`
when no project is connected. Check if `active_persona` is set — if it is, the
`attempt_forward` function goes directly to `run_pm_task_with_persona` (line 460–469)
which is the correct path. If `active_persona` is `None` and the socket probe
succeeds, it goes to `forward_to_controller` which routes through
`run_pm_task_with_history`.

---

## Issue 2: Unacceptably slow requests

### Call chain for ctrl direct chat (OAuth / claude-code path)

```
User submits message
  → tui.rs:477-508: sets thinking=true, spawns background task
  → repl/mod.rs:handle_input (line 1148)
  → repl/mod.rs:attempt_forward (line 459)
    [case: socket alive]
    → ctrl::forward_to_controller (ctrl/mod.rs:1750)
      → writes task JSON to socket
      → handle_socket_connection reads it
      → run_pm_task_with_history (ctrl/mod.rs:865)
        → events::publish(PmThinking)        ← line 884
        → resolve_agent_config()             ← disk I/O
        → pick_credentials()
        → apply_credential_routing()
        → build_deployment_footer()
        → run_pm_task_via_claude_cli()       ← if claude_cli_short_circuit
          → ClaudeCodeAgentRunner::new()     ← spawns `claude` subprocess
            → events::publish(AgentStarted) ← line 259
          → run_with_config_public()         ← full claude CLI invocation
```

### Identified performance problems

**1. Double LLM call via socket round-trip (most impactful)**

When the controller socket is alive, the message goes:
- REPL → socket → controller process → `run_pm_task_with_history` → PM LLM call

This is TWO hops for what should be a direct ctrl call. The ctrl agent's response
requires a full PM orchestration round-trip through `run_pm_task_with_history`, which
loads pm.toml and runs the PM (not ctrl) agent. The user is talking to the PM
orchestrator instead of the ctrl agent directly.

For the `AgentScope::User` case (ctrl direct chat), the PM orchestrator should never
be involved. The socket path unconditionally routes to `run_pm_task_with_history`.

**2. `ClaudeCodeAgentRunner::new()` on every call**

`run_pm_task_via_claude_cli` creates a new `ClaudeCodeAgentRunner` on every invocation
(line 1465). This includes locating the `claude` binary, which is a filesystem scan.
No caching occurs.

**3. History serialization/deserialization**

The socket path at `forward_to_controller` serializes the full conversation history to
JSON, sends it over a Unix socket, and the socket handler deserializes it (line 1644).
For long conversations this adds latency.

**4. Disk I/O in `resolve_agent_config`**

`run_pm_task_with_history` calls `resolve_agent_config` on every invocation. This does
file existence checks and TOML parsing on every request. No caching.

**5. `build_deployment_footer` string construction + `push_str` on every call**

The system prompt is rebuilt from scratch on every call including all skills injection.

### Recommended fixes for slowness

**Fix A (highest impact): Route `AgentScope::User` in the socket handler**

The socket handler at `handle_socket_connection` should check whether the incoming
request has a scope field. For `AgentScope::User` requests, route to
`run_pm_task_with_persona("ctrl", …)` instead of `run_pm_task_with_history`. This
ensures direct ctrl chat never routes through the PM orchestrator.

**Fix B: Bypass the socket for `AgentScope::User` in-process**

In `attempt_forward` (repl/mod.rs:459), the case where `active_persona` is set already
correctly bypasses the socket and goes directly to `run_pm_task_with_persona`. The
`AgentScope::User` / no-persona path should also bypass the socket and go directly
to `run_pm_task_with_persona("ctrl", …)` without probing the socket at all.

Current code (line 471–479):
```rust
match CtrlSocket::probe_default(&self.socket_path).await {
    Ok(stream) => {
        // routes ALL requests to socket, including ctrl User scope
        ctrl::forward_to_controller(stream, …).await?
    }
    Err(_) => {
        // only on socket failure do we check scope
        match self.current_scope() { … }
    }
}
```

The socket probe branch should also check scope:
```rust
match CtrlSocket::probe_default(&self.socket_path).await {
    Ok(stream) if self.current_scope() == AgentScope::Project => {
        ctrl::forward_to_controller(stream, …).await?
    }
    _ => {
        match self.current_scope() {
            AgentScope::User => run_pm_task_with_persona("ctrl", …),
            AgentScope::Project => run_pm_task_with_history(…),
        }
    }
}
```

This would reduce ctrl direct chat to a single in-process LLM call with no socket
overhead.

**Fix C: Cache `ClaudeCodeAgentRunner`**

Cache the runner (or at least the resolved binary path) at startup. The `new()` call
does a binary search on every dispatch.

---

## Key File Locations

| File | Lines | Issue |
|---|---|---|
| `src/ctrl/mod.rs` | 884 | `PmThinking` emit — first thing `run_pm_task_with_history` does |
| `src/ctrl/mod.rs` | 1660 | Socket handler always calls `run_pm_task_with_history` |
| `src/ctrl/mod.rs` | 1465 | `ClaudeCodeAgentRunner::new()` created on every ctrl call |
| `src/repl/mod.rs` | 459–510 | `attempt_forward` — socket probe doesn't respect `AgentScope::User` |
| `src/repl/mod.rs` | 1044 | `PmThinking` → `"PM thinking…"` text mapping in thinking relay |
| `src/repl/tui.rs` | 768–769 | Default `"thinking…"` text when `thinking_lines` is empty |
| `src/agents/claude_code_runner.rs` | 259 | `AgentStarted` emit (shows `pm · running (claude-code)…`) |
