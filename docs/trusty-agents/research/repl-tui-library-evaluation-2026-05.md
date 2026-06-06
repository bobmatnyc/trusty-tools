# REPL TUI Library Evaluation — Persistent-Bar + Scrollback Chat

**Date**: 2026-05-01
**Context**: Replaces the manual `crossterm` cursor-positioning code in
`src/repl/` (mod.rs, input.rs, status_bar.rs, banner.rs). The previous
`repl-ui-evaluation.md` (2026-04-25) covered reedline for line-editing only.
This document covers the full-layout problem: persistent input bar pinned to
the bottom, scrollback chat above it, startup banner box, thinking indicator,
and subprocess output interleaving.

---

## Problem Summary

The current implementation manually manages terminal layout across ~3,600 LOC
in `src/repl/`. The fragility is structural:

- `cursor::position()` is called at runtime to compute `bar_top`; any async
  output that lands during that query corrupts the offset.
- `ScrollDown(n)` is used to push content up before drawing; the correct `n`
  must be calculated from the current cursor row, which is wrong if anything
  printed between reads.
- Sub-agent stderr bleeds in because it shares the same fd as the bar draws.
- Bug references #260 and #261 are baked into comments throughout
  `src/repl/input.rs` as ongoing workarounds, not root-cause fixes.

The fundamental issue: absolute-coordinate cursor positioning on a terminal
that can scroll unpredictably is not a solvable problem without owning the
full screen render cycle.

---

## Comparison Table

| Criterion | `ratatui` 0.29 | `rustyline` / `reedline` | `crossterm` + TerminalUI abstraction | `inquire` / `dialoguer` |
|---|---|---|---|---|
| **What it is** | Full-screen TUI framework (widget-based, owns the screen) | Line-editor only (readline replacement) | Current approach, just better structured | Form/prompt library (wizard-style) |
| **Persistent bottom bar** | Native: `Layout::vertical` with fixed-height footer constraint | Not applicable — single-line input only | Requires manual `MoveTo` (same bugs) | Not applicable |
| **Scrollable chat area** | Native: `Paragraph` + scroll offset, auto-wraps | Not applicable | Requires manual scroll tracking | Not applicable |
| **Banner / box widget** | Native: `Block` with borders + `Paragraph` inside | Not applicable | Manual draw (current approach) | Not applicable |
| **Thinking indicator** | Trivial: swap a `Span` on the footer `Paragraph` | Requires `ExternalPrinter` + coord math | Requires `draw_static(true)` re-render path | Not applicable |
| **Slash autocomplete** | Via `tui-textarea` or custom `EventHandler` | reedline has it natively | Manual | Not applicable |
| **Async / tokio** | Sync render loop in `spawn_blocking`; event polling via crossterm's async `EventStream` | `spawn_blocking` for `read_line()` | Same as today | N/A |
| **Sub-process output interleaving** | Safe: channel messages into app state, re-render on tick | Fragile: same `ExternalPrinter` gap | Fragile: same root problem | N/A |
| **Eliminates cursor-position bugs** | Yes — ratatui owns the full buffer; no `cursor::position()` calls needed | No — only affects the input line | No — still requires manual coord math | N/A |
| **Already in Cargo.toml** | Yes (`ratatui = "0.29"`) | reedline not yet | Yes (`crossterm = "0.28"`) | No |
| **Crate maturity** | ~3.5M downloads, active, formerly tui-rs | 2.2M downloads (reedline); 32M (rustyline) | crossterm is mature; abstraction is new | Mature but wrong scope |
| **Migration effort** | 4–6 days (full rewrite of `src/repl/`) | 1–2 days but only fixes input line | 2–3 days but does not fix root cause | Not viable |

---

## Recommendation: Migrate to `ratatui`

**ratatui is the right answer.** It is the only option that eliminates the
cursor-positioning bug class at the root. The others are either scoped to
line-editing (reedline), preserve the broken model with better structure
(crossterm abstraction), or are irrelevant (inquire/dialoguer).

### Why ratatui solves it permanently

ratatui's rendering model is "declare the full screen state, call
`terminal.draw(|f| ...)`, done." The framework computes diffs and emits the
minimum ANSI sequences — no `cursor::position()` queries, no `ScrollDown`
calculations, no bar-top geometry. Sub-process output cannot bleed in because
nothing writes to the terminal except the render loop.

The layout for the REPL maps directly to standard ratatui patterns:

```
┌─────────────────────────────────────────────────────┐  ← terminal top
│  Banner (startup only; dismisses after first input) │  Constraint::Length(10)
├─────────────────────────────────────────────────────┤
│                                                     │
│  Chat scrollback (Paragraph, scroll_offset)         │  Constraint::Min(0)
│  ❯ user text (green)                                │
│  ⏺ response (orange)                               │
│                                                     │
├─────────────────────────────────────────────────────┤
│  ctrl> ▊  [thinking...]                             │  Constraint::Length(3)
└─────────────────────────────────────────────────────┘  ← terminal bottom
```

Each region is a `Paragraph` inside a `Block`. The thinking indicator is a
conditional `Span` appended to the footer paragraph — no separate render path.
Scroll offset is an `usize` in app state, incremented on new messages.

### open-mpm-specific concerns

**Async integration**: ratatui's render loop is synchronous (it calls
`terminal.draw` on each tick), but all mutable state lives in an `AppState`
struct. Background tokio tasks (PM output, sub-agent results) send `Event`
variants over an `mpsc::UnboundedSender`. The event loop reads from that
channel on each tick and updates `AppState` before rendering. This is the
standard ratatui async pattern — `src/debugger/tui.rs` already implements it.

**Sub-process output interleaving**: Because all output goes through the
channel into `AppState.messages`, sub-agent stderr (currently inherited) must
be captured and forwarded through the same channel. This is a one-time
plumbing change that fixes the bleed-in permanently.

**Alternate screen**: ratatui runs in the alternate screen (`EnterAlternateScreen`)
by default. The REPL today does not. This is the correct behavior — the user's
scrollback terminal history is preserved on exit. If "inline" (non-alternate)
mode is required, ratatui supports it by skipping `EnterAlternateScreen`, but
scroll behavior becomes terminal-dependent again.

**Precedent in this codebase**: `src/debugger/tui.rs` is already a working
ratatui implementation with split panes, scroll, and a footer. The REPL
migration can copy that structure directly.

### Migration effort

| Phase | Work | Estimate |
|---|---|---|
| 1. App state + event channel | `AppState`, `Event` enum, `mpsc` wiring | 0.5 day |
| 2. Layout + widgets | Chat `Paragraph`, footer input bar, banner | 1 day |
| 3. Input handling | Replace `InputBox::read_line` with ratatui event loop | 1 day |
| 4. Sub-process output capture | Redirect agent stderr through event channel | 0.5 day |
| 5. Delete old code | Remove `src/repl/input.rs`, `status_bar.rs`, `banner.rs` cursor logic | 0.5 day |

**Total: ~3.5 days.** The existing `src/debugger/tui.rs` is a reference
implementation; most of the boilerplate is already written and can be adapted.

### Why not crossterm + TerminalUI abstraction

A `TerminalUI` struct centralizing all cursor operations reduces the call-site
count but does not fix the underlying fragility. The struct still has to call
`cursor::position()` at runtime to calculate `bar_top`, and that query is
still racy with async output. Every new feature that touches the layout
(thinking indicator, banner visibility toggle, status bar) requires new
coordinate math. The bug surface does not shrink — it consolidates.

### Why reedline alone is insufficient

reedline (recommended in the 2026-04-25 evaluation) solves the input line.
It does not manage the chat area above it, the banner, or the thinking
indicator. Those still require manual crossterm draws, which is where the bugs
live. The two tools are complementary if staying on crossterm; but if migrating
to ratatui, ratatui's own event loop replaces reedline's `read_line` call.

---

## Relationship to Previous Evaluation

The 2026-04-25 `repl-ui-evaluation.md` recommended reedline for the line-editor
specifically, targeting the `src/ctrl/` raw stdin loop. That recommendation
remains valid if the scope is line-editing only (history, tab completion).

This evaluation addresses the higher-level layout problem in `src/repl/`.
The two scopes are compatible: ratatui can host a reedline-style input widget,
or handle raw key events directly (which is simpler given ratatui's event loop
already processes `KeyEvent`s).

---

## Decision

Migrate `src/repl/` to ratatui. Delete the manual cursor-positioning code.
Use `src/debugger/tui.rs` as the structural template.
