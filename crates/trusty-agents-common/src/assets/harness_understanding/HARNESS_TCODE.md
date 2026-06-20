# Harness Understanding — tcode (trusty-code) Specifics

> DOC-21 `SPEC-HARNESS-UNDERSTANDING-01~draft` | Section: tcode

tcode (the `trusty-code` harness, binary `tcode`) differs from Claude Code
in that it emits **structured NDJSON event lines** in addition to pane text.

## `__HARNESS_EVENT__` NDJSON Events

tcode emits events as NDJSON lines prefixed with `__HARNESS_EVENT__ `:

```
__HARNESS_EVENT__ {"source":"code","seq":1,"payload":{"domain":"lifecycle","event":{"type":"session_started","session_id":"s1","project":"my-repo"}}}
```

To parse: strip the `__HARNESS_EVENT__ ` prefix (21 chars including trailing
space), then parse the remainder as JSON as a `HarnessEvent` envelope.

Key event types to watch:
- `session_started` / `session_ended` — lifecycle transitions
- `pm_thinking` — agent is reasoning (Working state)
- `agent_message` — agent produced output (may be Working or completion)
- `recap_generated` — session recap; treat as completion evidence candidate

## Task Banners

tcode emits task-start and task-end banners in the pane:
- Task start: lines matching `=== Task: <description> ===` or similar
- Task end: `=== Done ===`, `=== Completed ===`, or the `recap_generated` event

## Agent-Delegation Output

When tcode delegates to an agent, the pane shows agent output prefixed with
the agent name. Example:
```
[rust-engineer] Starting implementation...
[rust-engineer] cargo check: Finished
```

Delegation lines are Working signals. Completion of the delegated task
produces a summary line (the agent's final output) followed by the
`agent_message` event.

## Diff / Edit Confirmation

tcode confirms file edits with structured diff output:
```
Edit: src/foo.rs (+15, -3)
Write: src/bar.rs (new file, 42 lines)
```

These patterns are completion evidence for "edit made" claims.

## State Classification for tcode

Apply the agnostic model, but prefer `__HARNESS_EVENT__` events over pane text
when available:

| Event / pane signal | State |
|--------------------|-------|
| `pm_thinking` event | Working |
| `agent_message` event (task ongoing) | Working |
| `recap_generated` event | Done (completion evidence) |
| `session_ended` event | Stopped |
| Pane silent, no events for >30s | Idle (apply agnostic fallback) |
| Error events or `error:` pane text | Error |
