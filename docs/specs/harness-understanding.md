# DOC-21 — Harness Understanding: canonical mental model for SM and future t-code overseer

**Status:** Draft
**Subsystem:** trusty-agents-common — harness-understanding instructions; trusty-mpm — SM prompt
**Owner:** Engineering (trusty-agents-common, trusty-mpm)
**Last-updated:** 2026-06-20
**Spec ID:** `SPEC-HARNESS-UNDERSTANDING-01~draft` (DOC-21)
**Builds on:** DOC-14 — Session Manager (SM) Agent (`docs/specs/session-manager-agent.md`); DOC-17 — Autonomous Multi-Session Managed Harness Runner (`docs/specs/harness-runner-vision.md`); DOC-20 — Chat-Core (`docs/specs/chat-core.md`)
**Cross-ref:** `crates/trusty-agents-common/src/harness_doc.rs`, `crates/trusty-agents-common/src/assets/harness_understanding/`, `crates/trusty-mpm/src/core/sm/prompt.rs`, `crates/trusty-mpm/src/assets/sm_instructions/`, issues **#1510**, **#862**, **#875**

> **Scope note.** This is a behavior-contract spec for the *canonical harness-understanding instruction set* shared by trusty-agents-common and consumed by trusty-mpm's session-manager (SM) prompt today and a future t-code overseer tomorrow. It defines the harness mental model, per-harness signals, the intervention decision protocol, a deterministic LLM-free fallback, SM-specific wiring, and the forward-looking t-code-as-overseer contract.

---

## 1. Motivation

The current gap: `SM_WORKFLOW` tells the SM to "interpret the raw pane state yourself" but never teaches *how*. No harness mechanics are encoded anywhere the SM can reason from. Additionally, the `ActivityMonitor` degrades to `ActivityState::Unknown` when `OPENROUTER_API_KEY` is absent, leaving the SM with no deterministic fallback to classify sessions.

This spec fills the gap by defining:
1. A canonical harness mental model (session lifecycle, pane/IO model, prompt shapes, tool-call patterns, approval gates, completion evidence, error patterns).
2. A WHEN-TO-INTERVENE decision protocol mapping observable state to SM actions.
3. A deterministic LLM-free fallback that applies directly to raw pane text.
4. Per-harness specifics for Claude Code and tcode.
5. trusty-mpm SM wiring: how OBSERVE/VERIFY consume the model.
6. Forward-looking t-code-as-overseer contract.

The shared module in `trusty-agents-common` ensures both the SM prompt and a future t-code overseer consume the same authoritative content.

---

## 2. Canonical Harness Mental Model (Agnostic)

### 2.1 Session Lifecycle & States

Every harness session passes through a well-defined lifecycle mapping to `HarnessState`:

| State | Meaning | Observable signals |
|-------|---------|-------------------|
| `Starting` | Process launched, not yet producing output | Blank pane or very first output lines |
| `Idle` | Ready and waiting for operator input | Prompt visible (`> `, `$`, `#`), no activity |
| `Working` | Actively processing a task | Spinner glyphs, streaming output, tool calls |
| `Paused` | Waiting on an operator decision | A question, permission dialog, or approval gate |
| `Error` | Encountered a non-recoverable error | Error banners, exit codes, panic traces |
| `Stopped` | Session has exited cleanly | No runtime process; final output visible |

### 2.2 Pane / IO Model

The **pane** is the tmux-backed terminal surface — it is the ONLY observable surface for a running harness. The SM reads captured pane text (observe) and writes to it (send a message).

Key facts:
- The SM sees the **last N lines** of the terminal (the observation window).
- Output is **streaming** — the pane may be mid-output when observed.
- A pane silent for >30 seconds with no Working glyphs is almost certainly Idle or Stopped.

### 2.3 Prompt Shapes

| Harness | Idle prompt | Notes |
|---------|------------|-------|
| Claude Code | `> ` (bare `>` at start of line) | Single `>` alone on the line |
| Shell | `$ ` or `# ` | Standard Unix shell prompts |
| tcode | No pane prompt (event-driven) | Uses `__HARNESS_EVENT__` NDJSON events |

An idle prompt on the **last line** of the pane is the strongest signal that a harness is Idle.

### 2.4 Tool-Call Patterns

- **Claude Code**: Tool calls appear as bracketed blocks. The `✻` glyph appears during tool execution (`✻ Running bash...`, `✻ Reading file...`).
- **tcode**: Emits structured NDJSON event lines prefixed with `__HARNESS_EVENT__`. Example: `__HARNESS_EVENT__ {"type":"tool_use","tool":"bash","input":{...}}`. Parseable as JSON after stripping the prefix.
- **Shell**: Raw command output; no structured wrapper.

### 2.5 Approval / Decision Gates

Harnesses may pause and wait for operator approval:
- **Permission dialogs**: Claude Code shows `Allow` / `Deny` / `Always allow` options — hard blockers.
- **Y/N prompts**: Text ending with `? [y/N]` or `Continue? (y/n):`.
- **Trust dialogs**: First-run trust establishment.
- **Custom questions**: Open-ended operator decisions.

When a decision gate is visible, the session is in `Paused` state.

### 2.6 Completion & Evidence Signals

A task is done only when evidence is observed:

| Claim | Evidence pattern |
|-------|-----------------|
| "PR opened" | GitHub PR URL: `https://github.com/.*/pull/\d+` |
| "tests pass" | `N passed; 0 failed`, `test result: ok. N passed`, `All tests passed` |
| "edit made" | Diff output or `Write(path)` / `Edit(path)` confirmation lines |
| "task done" | `Task complete`, `Done`, `✓`, or equivalent success banner |
| "build passes" | `Finished` or `warning(0)` from `cargo check` / `cargo build` |

Never claim a task is complete without at least one observed evidence pattern.

### 2.7 Error Patterns

- **Rust compilation errors**: `error[E`, `error:` with file:line citations
- **Test failures**: `FAILED`, `N failed`, `test result: FAILED`
- **Runtime panics**: `thread 'main' panicked at`, `panicked at '`
- **Process exit errors**: `exit code: N` (non-zero), `exited with status`
- **Generic prefixes**: Lines starting with `Error:` or `ERROR:`
- **Tool errors**: `✻ Error:` or structured error blocks in Claude Code

### 2.8 Observability & Inference

Two tiers:
1. **Deterministic (LLM-free)**: Apply patterns to raw pane text. No tokens, no network, always available.
2. **LLM-backed (optional)**: `ActivityMonitor` with `OPENROUTER_API_KEY`. Returns `ActivityState` with confidence. When key absent: `state = Unknown` — fall back to tier 1.

Always apply tier 1 first; tier 2 can confirm or refine but never overrides a clear tier-1 signal.

---

## 3. WHEN-TO-INTERVENE Decision Protocol

The canonical decision tree for every pane observation:

| Observed state | Action | Rationale |
|----------------|--------|-----------|
| `Working` with progress signals | **WAIT** | Session is making progress |
| `Idle` + pending question visible | **SEND** answer | Session is blocked |
| `Idle` + no question, task incomplete | **SEND** follow-up | Session may be stuck |
| `Paused` + permission dialog | **ESCALATE** or send authorized answer | May require operator judgment |
| `Error` | **INTERRUPT** + report error | Cannot self-recover |
| Silent >5 min, Unknown state | **INTERRUPT** + re-observe | Abnormal silence |
| `Stopped` (runtime exited) | **REPORT** result + check evidence | Apply verification gate |

**Decision escalation order**: WAIT < SEND < INTERRUPT < ESCALATE (FlagForHuman).
Choose the lowest-intensity action that resolves the observable state.

---

## 4. Deterministic (LLM-Free) Fallback Model

When `OPENROUTER_API_KEY` is absent (or the LLM returns `Unknown`), apply these rules to raw pane text:

### idle-by-silence
- Pane silent for >30 seconds, AND no Working glyphs in the last 5 lines, AND last non-empty line matches `^>\s*$`, `^\$\s`, or `^#\s`
→ **Classify as Idle**

### completion-by-banner
- Pane contains: PR URL, test-pass summary, `Task complete`, `Done.`, `✓ Complete`, or `Finished` (Rust build)
→ **Classify as Done / Idle with completion evidence**

### error-by-keyword
- Pane contains: `error[E`, `FAILED`, `panicked at`, `exit code: [^0]`, `Error:` (line-start), `✻ Error:`
→ **Classify as Error**

### working-by-glyph
- Pane contains `✻` in the last 10 lines, OR output is actively streaming
→ **Classify as Working**

### default
- None of the above → **Classify as Unknown** (re-observe before acting)

This directly addresses the `ActivityState::Unknown` fallback gap (issue #875).

---

## 5. Per-Harness Specifics

### 5.1 Claude Code

- **Banner**: `Claude Code vX.Y` (startup)
- **Working glyph**: `✻`
- **Idle prompt**: `> ` (single `>` alone on line)
- **Permission dialogs**: Shows `Allow`, `Deny`, `Always allow` options
- **Completion patterns**: `Task complete`, PR URL, diff output
- **Error patterns**: `✻ Error:`, `error[E`, compilation errors

### 5.2 tcode

- **Task banners**: `=== Task: <description> ===` (start), `=== Done ===` (end)
- **Event prefix**: `__HARNESS_EVENT__` NDJSON lines — parseable events
- **Agent-delegation output**: `[agent-name] <output>` prefixed lines
- **Diff/edit confirmation**: `Edit: src/foo.rs (+15, -3)`, `Write: src/bar.rs`
- **State classification**: Prefer `__HARNESS_EVENT__` events over pane text

---

## 6. trusty-mpm Session Manager Wiring

### 6.1 How OBSERVE Consumes This

`sessions.get(session_id)` returns a `RawObservation` with `raw_pane`, `runtime_active`, `pending_decision`, and `proposed_default`. The SM:
1. Checks `runtime_active` — if `false`, session has Stopped.
2. Checks `pending_decision` — if set, session is Paused.
3. Applies the deterministic model (§4) to `raw_pane`.
4. Optionally uses the LLM `Summary` to confirm tier-1 classification.

### 6.2 How VERIFY Uses §2.6 Completion Evidence Signals

The BLOCKING verification gate (SM Workflow §5) maps completion claims to observed evidence in `raw_pane`. No evidence → task not done. Re-observe, then send follow-up.

### 6.3 Managed States and Adopt Semantics

On adopt of a pre-existing tmux session, the SM has no prior history. Apply the full observation protocol from scratch. If `pending_decision` is set, handle as Paused.

### 6.4 Chat-Core Verbs and §3 Mapping

| §3 action | SM verb |
|-----------|---------|
| WAIT | (no call; next poll cycle) |
| SEND answer | `sessions.send(session_id, text)` |
| INTERRUPT | `sessions.stop(session_id)` then re-launch |
| ESCALATE | Report to operator; do NOT send to session |

### 6.5 RawObservation/Summary Two-Tier

- **`observe` (raw, always available)**: `sessions.get` with no inference. Uses deterministic model.
- **`summarize` (LLM-backed, optional)**: When `OPENROUTER_API_KEY` is present. When absent: `state = Unknown`, `summary = None`, `confidence = 0.0`.

Operate from `RawObservation` alone when needed; LLM summary is a bonus.

### 6.6 Override Convention

`~/.trusty-mpm/sm/SM_HARNESS.md` overrides the shared trusty-agents-common harness section. When absent or empty, the shared content from `trusty-agents-common::harness_doc` is used. Follows the same convention as `SM_INSTRUCTIONS.md`, `SM_WORKFLOW.md`, and `SM_TOOLS.md`.

---

## 7. Forward-Looking t-code-as-Overseer Contract

### 7.1 The Seam

Two components already in place:
1. **`Overseer` trait** (`crates/trusty-mpm/src/core/overseer.rs`): dispatches hook events; returns `OverseerDecision::{Allow, Block, Respond, FlagForHuman}`.
2. **`HarnessSource::Code`** (`crates/trusty-agents-common/src/events/lifecycle.rs`): reserved source tag for tcode-originated `HarnessEvent`s.

### 7.2 Event-to-Decision Mapping

A t-code overseer maps `HarnessEvent`s (filtered on `HarnessSource::Code`) to `OverseerDecision` using the §3 WHEN-TO-INTERVENE protocol:

| Observed event | Decision | Rationale |
|----------------|----------|-----------|
| `pm_thinking` — routine reasoning | `Allow` | Normal Working state |
| `agent_message` — within scope | `Allow` | Expected output |
| Permission dialog detected | `Respond` or `FlagForHuman` | Decision gate |
| Error + non-recoverable | `FlagForHuman` | Human must decide |
| Sensitive file write outside scope | `Block { reason }` | Scope violation |
| Unexpected tool call | `Block { reason }` | Scope enforcement |
| Long silence (>5 min) in Working state | `FlagForHuman` | Possible hang |

### 7.3 `__HARNESS_EVENT__` Lines as Structured Overseer Input

For a t-code overseer, `__HARNESS_EVENT__` lines are the primary structured input (vs. pane text for Claude Code). Strip the prefix, parse as `HarnessEvent`, filter on `source == HarnessSource::Code`.

### 7.4 Reserved `HarnessSource::Code` Slot

No emitters exist yet. This spec governs the behavioral contract when the first real emitter lands. The `HarnessSource::Code` tag transitions from "reserved" to "active" at that point.

---

## 8. Where It Lives

The shared module lives in:
- **Module**: `crates/trusty-agents-common/src/harness_doc.rs`
- **Assets**: `crates/trusty-agents-common/src/assets/harness_understanding/`
  - `HARNESS_AGNOSTIC.md` — §2 + §3 + §4 content
  - `HARNESS_MPM_SM.md` — §6 content
  - `HARNESS_TCODE.md` — §5.2 content
  - `HARNESS_OVERSEER.md` — §7 content

**Consumers load it via**:
- `trusty-mpm`: `use trusty_agents_common::harness_doc; harness_doc::harness_understanding()`
  called from `crates/trusty-mpm/src/core/sm/prompt.rs`.
- Future tcode overseer: same API, filtered to relevant sections.

---

## References

- DOC-14: Session Manager (SM) Agent — `docs/specs/session-manager-agent.md`
- DOC-17: Autonomous Multi-Session Managed Harness Runner — `docs/specs/harness-runner-vision.md`
- DOC-20: Chat-Core — `docs/specs/chat-core.md`
- Issues: #1510 (this spec), #862 (trusty-agents-common extraction), #875 (ActivityState::Unknown fallback)

---

## Changelog

| Date | Change |
|------|--------|
| 2026-06-20 | Initial draft (DOC-21, #1510) |
