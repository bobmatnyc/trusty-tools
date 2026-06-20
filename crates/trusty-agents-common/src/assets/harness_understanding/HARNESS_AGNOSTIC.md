# Harness Understanding — Canonical Mental Model

> DOC-21 `SPEC-HARNESS-UNDERSTANDING-01~draft` | Section: Agnostic Core

This section encodes the canonical mental model for any AI coding harness that
trusty-mpm manages. Read this before interpreting any session pane.

## Session Lifecycle & States

Every harness session passes through a well-defined lifecycle:

| State | Meaning | Observable signals |
|-------|---------|-------------------|
| `Starting` | Process launched, not yet producing output | Blank pane or very first output lines |
| `Idle` | Ready and waiting for operator input | Prompt visible (`> `, `$`, `#`), no activity |
| `Working` | Actively processing a task | Spinner glyphs, streaming output, tool calls |
| `Paused` | Waiting on an operator decision | A question, permission dialog, or approval gate |
| `Error` | Encountered a non-recoverable error | Error banners, exit codes, panic traces |
| `Stopped` | Session has exited cleanly | No runtime process; final output visible |

## Pane / IO Model

The **pane** is the tmux-backed terminal surface — it is the ONLY observable
surface for a running harness. You interact with it by reading captured output
(observe) and writing to it (send a message).

Key facts about panes:
- You see the **last N lines** of the terminal (the observation window).
- Output is **streaming** — the pane may be mid-output when you observe it.
- **Scroll position does not matter** — the observation window always shows
  the most recent lines.
- A pane that has been silent for >30 seconds with no Working glyphs is
  almost certainly Idle or Stopped.

## Prompt Shapes

Different harnesses show different idle prompt shapes:

| Harness | Idle prompt | Notes |
|---------|------------|-------|
| Claude Code | `> ` (bare `>` at start of line) | Single `>` alone on the line |
| Shell | `$ ` or `# ` | Standard Unix shell prompts |
| tcode | No pane prompt (event-driven) | Uses `__HARNESS_EVENT__` NDJSON events |

An idle prompt on the **last line** of the pane (or the last non-empty line)
is the strongest signal that a harness is Idle and waiting for input.

## Tool-Call Patterns

When a harness executes tools, the pane shows structured patterns:

- **Claude Code**: Tool calls appear as bracketed blocks with tool name and
  arguments, followed by a result block. The `✻` glyph often appears during
  tool execution. Example: `✻ Running bash...` or `✻ Reading file...`
- **tcode**: Emits structured NDJSON event lines prefixed with `__HARNESS_EVENT__`.
  Example: `__HARNESS_EVENT__ {"type":"tool_use","tool":"bash","input":{...}}`
  These lines are parseable as JSON after stripping the prefix.
- **Shell**: Raw command output; no structured wrapper.

## Approval / Decision Gates

Harnesses may pause and wait for operator approval:

- **Permission dialogs**: Claude Code shows dialog text with `Allow` / `Deny` /
  `Always allow` options. These are hard blockers — the session will not proceed
  until an answer is sent.
- **Y/N prompts**: Text ending with `? [y/N]` or `Continue? (y/n):` patterns.
- **Trust dialogs**: First-run trust establishment (e.g. `Trust this workspace?`).
- **Custom questions**: The harness may ask open-ended questions that require
  an operator decision before proceeding.

When you see a decision gate, the session is in `Paused` state. You must either
send an answer (if you are authorized to do so) or escalate to the operator.

## Completion & Evidence Signals

A task is done only when you observe **evidence**. The canonical evidence patterns:

| Claim | Evidence pattern in pane |
|-------|--------------------------|
| "PR opened" | A GitHub PR URL: `https://github.com/.*/pull/\d+` |
| "tests pass" | Test summary: `N passed; 0 failed`, `test result: ok. N passed`, `All tests passed` |
| "edit made" | Diff output or `Write(path)` / `Edit(path)` confirmation lines |
| "task done" | `Task complete`, `Done`, `✓`, or equivalent success banner |
| "build passes" | `cargo check` / `cargo build` success: `Finished` or `warning(0)` |

**Never claim a task is complete without at least one observed evidence pattern.**

## Error Patterns

Errors produce recognizable output in the pane:

- **Rust compilation errors**: `error[E`, `error:`, followed by file:line citations
- **Test failures**: `FAILED`, `N failed`, `test result: FAILED`
- **Runtime panics**: `thread 'main' panicked at`, `panicked at '`
- **Process exit errors**: `exit code: N` (non-zero), `exited with status`
- **Generic error prefixes**: Lines starting with `Error:` or `ERROR:`
- **Tool errors**: `✻ Error:` or structured error blocks in Claude Code

When you observe error patterns, classify the session as `Error` and include
the error text in your report. **Do not assume recovery happened unless you see
a subsequent success signal.**

## Observability & Inference

Two tiers of observation are available:

1. **Deterministic (LLM-free)**: Apply the patterns in this section directly to
   raw pane text. No tokens consumed, no network, always available. Described in
   the fallback model below.

2. **LLM-backed (optional)**: The `ActivityMonitor` classifies pane content
   using an LLM when `OPENROUTER_API_KEY` is configured. Returns `ActivityState`
   with a confidence score. When the key is absent, the classifier returns
   `state = Unknown` — fall back to tier 1.

Always apply tier 1 first. Tier 2 can confirm or refine, but never contradicts
a clear tier-1 signal (e.g. an explicit PR URL in the pane is completion
evidence regardless of the LLM's classification).

## Deterministic LLM-Free Fallback Model

When `OPENROUTER_API_KEY` is absent (or the LLM returns `Unknown`), apply these
deterministic rules to raw pane text:

### idle-by-silence
- Pane has not changed for >30 seconds, AND
- No Working glyphs (`✻`, spinner characters) in the last 5 lines, AND
- Last non-empty line matches an idle prompt pattern (`^>\s*$`, `^\$\s`, `^#\s`)
→ **Classify as Idle**

### completion-by-banner
- Pane contains any of: PR URL (`https://github.com/.*/pull/\d+`), test-pass
  summary (`\d+ passed.*0 failed`, `All \d+ tests passed`), `Task complete`,
  `Done.`, `✓ Complete`, `Finished` (Rust build)
→ **Classify as Done / Idle with completion evidence** (report the evidence)

### error-by-keyword
- Pane contains any of: `error[E`, `FAILED`, `panicked at`, `exit code: [^0]`,
  `Error:` (line-start), `✻ Error:`
→ **Classify as Error** (include the error excerpt in your report)

### working-by-glyph
- Pane contains `✻` in the last 10 lines, OR output is actively streaming
  (multiple new lines since last observation)
→ **Classify as Working**

### default
- None of the above → **Classify as Unknown** (report honestly; consider waiting
  and re-observing before acting)

## WHEN-TO-INTERVENE Decision Protocol

Use this protocol every time you observe a session pane:

| Observed state | Action | Rationale |
|----------------|--------|-----------|
| `Working` with progress signals | **WAIT** | Session is making progress; intervention disrupts work |
| `Idle` + pending question visible | **SEND** answer | Session is blocked waiting for your input |
| `Idle` + no question, task incomplete | **SEND** follow-up | Session may be stuck or awaiting next instruction |
| `Paused` + permission dialog | **ESCALATE** or send authorized answer | Permission gates may require operator judgment |
| `Error` | **INTERRUPT** + report error | Session cannot self-recover from hard errors |
| Silent >5 min, Unknown state | **INTERRUPT** + re-observe | Long silence without signals is abnormal |
| `Stopped` (runtime exited) | **REPORT** result + check evidence | Session is done; apply verification gate |

**Decision escalation order**: WAIT < SEND < INTERRUPT < ESCALATE (FlagForHuman).
Choose the lowest-intensity action that resolves the observable state.
