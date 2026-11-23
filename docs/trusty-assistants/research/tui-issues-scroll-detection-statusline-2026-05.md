# TUI Issues: Chat Scroll, claude-mpm Detection, Statusline Style Segment

**Date**: 2026-05-05
**Scope**: Three bug/feature investigations in `src/repl/tui.rs`, `src/adapters/`, `src/tm/`

---

## Issue 1: Chat Scroll-Up Support

### Current Implementation

**Data structure** (`src/repl/tui.rs:119-357`):
- `ReplApp.chat: Vec<ChatLine>` — flat list of `ChatLine { role: ChatRole, text: String }` entries
- `ReplApp.scroll_offset: usize` — lines scrolled up from bottom (0 = pinned to newest, line 127)

**Scroll state management** (`src/repl/tui.rs:704-711`):
```rust
pub fn scroll(&mut self, delta: isize) {
    if delta < 0 {
        self.scroll_offset = self.scroll_offset.saturating_add((-delta) as usize);
    } else {
        self.scroll_offset = self.scroll_offset.saturating_sub(delta as usize);
    }
}
```
Negative delta = scroll up (older), positive = scroll down (newer).

**Scroll_offset reset points** (lines 510, 555, 588):  
`scroll_offset = 0` is called in `push_user()`, `push_assistant()`, and `push_status()` — i.e., every new message auto-scrolls back to bottom.

**Key event handling** (`src/repl/tui.rs:1354-1361`):
```rust
KeyCode::PageUp   => { app.scroll(-10); None }
KeyCode::PageDown => { app.scroll(10);  None }
```
PageUp and PageDown are wired. They work correctly.

**Draw path** (`src/repl/tui.rs:2947-3285`):
```rust
// draw_chat, lines 3275-3284
let max_offset = final_total.saturating_sub(visible);
let effective_offset = max_offset.saturating_sub(app.scroll_offset);
let paragraph = Paragraph::new(final_lines)
    .wrap(Wrap { trim: false })
    .scroll((effective_offset as u16, 0));
```
The math is: `effective = max_offset - scroll_offset`. When `scroll_offset = 0`, the view is pinned to newest (offset = max). When `scroll_offset > 0`, the view shifts up.

**Mouse scroll** (`src/repl/tui.rs:809-828`):
The key reader thread at line 809 handles `CtEvent::Key` and `CtEvent::Resize`, but all other events fall through to `Ok(_) => continue`. Mouse scroll events (`CtEvent::Mouse(MouseEvent { kind: MouseEventKind::ScrollUp/ScrollDown })`) are **discarded**.

`EnableMouseCapture` is called at startup (line 851) so the terminal sends mouse events — they are simply not wired to `app.scroll()`.

### What's Missing / Broken

1. **Mouse scroll is dropped**: `CtEvent::Mouse(_)` hits the `Ok(_) => continue` arm at line 825 — no scroll delta is dispatched. Adding a `CtEvent::Mouse(m)` arm that maps `ScrollUp`→`ReplEvent::Key` or a dedicated `ReplEvent::Scroll(-3)`/`ReplEvent::Scroll(3)` would fix this.

2. **Scroll clamping — upper bound missing**: `scroll()` saturates at 0 on the downward path, but does NOT clamp `scroll_offset` to `max_offset`. In `draw_chat` at line 3280 the `saturating_sub` prevents negative `effective_offset`, so visually it's safe — but `scroll_offset` can grow unboundedly, causing the scroll position to appear "stuck" at the top for many extra PageDown presses before snapping back. The fix is to clamp in `scroll()` or in `draw_chat` before storing.

3. **No scroll indicator**: There's no visual affordance (scroll bar, `[n lines above]` hint, etc.) to tell the user they're scrolled up. Not strictly broken but contributes to the "no scroll" perception.

**PageUp/PageDown themselves work correctly** — the infrastructure is all there.

---

## Issue 2: claude-mpm Detection Broken in Tmux Sessions

### Detection Pipeline

1. **Reconcile** (`src/tm/manager.rs:288-338`): On startup and on `/tm list`, the TM manager calls `tmux.capture_output(session_name, None, Some(100))` for each newly discovered session, then passes the output to `self.adapters.detect(&pane_output)`.

2. **Capture call** (`src/tmux/orchestrator.rs:294-322`): `pane = None` means tmux targets the session's **active (last-focused) pane** with no explicit pane specifier. The tmux target string becomes just the session name (e.g., `"my-session"`), not `"my-session:0.0"`.

3. **AdapterRegistry.detect** (`src/adapters/registry.rs:83-105`): Iterates all non-shell adapters, requires confidence ≥ 0.7, returns highest-confidence match.

4. **ClaudeMpmAdapter.detect** (`src/adapters/claude_mpm.rs:70-76`):
```rust
pub fn detect(&self, pane_output: &str) -> DetectionResult {
    let window = last_n_lines(pane_output, 100);
    match best_match(&window, brand_patterns()) {
        Some(p) => DetectionResult::matched(p.confidence, p.name),
        None => DetectionResult::no_match(),
    }
}
```

5. **Brand patterns** (`src/adapters/claude_mpm.rs:22-32`):
   - `"PM ready"` → confidence 1.0
   - `r"(?i)claude-mpm"` → confidence 1.0
   - `r"(?i)\bMPM\b"` → confidence 0.9
   - `r"(?i)orchestrat"` → confidence 0.8
   - `r"(?i)delegat"` → confidence 0.7

### Root Cause of Detection Failure

**Primary issue — active pane vs. claude-mpm window**: When a tmux session has multiple windows or panes (e.g., an editor pane and a claude-mpm pane), `capture_output(..., None, ...)` captures the **active window/pane**. If the user's cursor is in a different window when reconcile runs, the captured output is from that window (e.g., a shell prompt), not the claude-mpm prompt. None of the brand patterns match and the session gets classified as `AdapterType::Shell`.

**Secondary issue — prompt pattern is too narrow**: The `"PM ready"` pattern requires the literal text "PM ready" to be visible in the last 100 lines. The open-mpm banner scrolls into the scrollback buffer quickly. After the first exchange, "PM ready" is no longer in the visible pane area captured by `capture-pane -S -100`. The remaining patterns (`orchestrat`, `delegat`) are very generic — "delegat" matches anything like "delegate", "delegation" that might appear in any text.

**The `open-mpm` adapter has the same problem** (`src/adapters/open_mpm.rs:23`): it relies on `"open-mpm"` being in the last 100 lines, which it won't be after the banner scrolls away.

**Test discrepancy**: The registry test at `src/adapters/registry.rs:205-208` uses `"PM ready\n> "` as sample input — this is the initial banner state, not the idle state after several exchanges.

### What's Missing / Broken

1. **No idle-state pane text that reliably identifies claude-mpm after startup**: Once the banner is gone, the pane shows a `> ` prompt which matches the idle pattern `r"(?m)^>\s*$"` (line 37 in `claude_mpm.rs`) — but this pattern is in `idle_patterns()`, not `brand_patterns()`. The `detect()` function only checks brand patterns.

2. **Pane capture target is session-level, not window-level**: In a multi-window session, the detected output may not come from the claude-mpm window at all.

3. **Detection is one-shot at reconcile time**: The adapter type is stored in the registry and not re-evaluated unless `detect_adapter()` is called explicitly. If the session was misidentified at startup, it stays wrong.

**Fix directions**:
- Add the idle prompt pattern `r"(?m)^>\s*$"` to `brand_patterns()` with moderate confidence (≥0.7), combined with session name hinting (e.g., if the session name contains "mpm" or "claude").
- Or: capture all panes of the session (iterate `list_panes`, capture each, union the outputs) rather than just the active pane.
- Or: re-detect on `/tm list` or on a timer.

---

## Issue 3: `style:claude_mpm` Statusline Segment

### Current Statusline Architecture

**Render path** (`src/repl/tui.rs:2200-2203`):
```rust
fn draw_statusline(f: &mut ratatui::Frame, app: &ReplApp, area: Rect) {
    let line = build_rich_statusline(app);
    f.render_widget(Paragraph::new(line), area);
}
```

**Segment builder** (`src/repl/tui.rs:2218-2300`): `build_rich_statusline(app)` assembles a `Line<'static>` from `Vec<Span>`. Current segments:
- `[open-mpm]` — always present, cyan bold
- `✓ LLM: <provider> (<model>)` — from `status_line`, bold rest
- `TM: <n> sessions` — from `app.tm_session_count`, injected after LLM chunk (line 2252-2288)
- `local: <model>` — from `app.local_model`, optional (line 2259-2262)
- `↑<k> ↓<k>` token counts — from `tokens_in`/`tokens_out` (line 2271)
- `$<cost>` or `<session> session` / `<daily> today` — cost segments (lines 2272-2276)
- `All systems go.` — trailing green text

**Chunk styler** (`src/repl/tui.rs:2388-2433`): `style_status_chunk(chunk)` dispatches by prefix:
- `"✓ LLM: "` → green checkmark + bold rest
- `"All systems go."` → green
- `'↑'` or `'$'` → unstyled
- `"TM: "` → dim label + bold count
- unknown → dim

**State that exists on `ReplApp`**:
- `tm_session_count: usize` (line 336) — total session count
- No per-adapter-type breakdown field exists

### What's Missing

**There is no `claude_mpm_count` or equivalent field on `ReplApp`**. The statusline receives only the total TM session count (`tm_session_count`) from `ReplEvent::TmSessionCount(usize)` (line 448). There is no event or field that carries "N of these sessions are claude-mpm type".

To add `style:claude_mpm`:

1. **New field on `ReplApp`**: Add `pub claude_mpm_session_count: usize` (alongside `tm_session_count` at line 336).

2. **New ReplEvent variant**: Add `ReplEvent::ClaudeMpmSessionCount(usize)` or extend `TmSessionCount` to carry per-adapter breakdowns.

3. **Startup wiring** (`src/repl/mod.rs:284-320`): After `reconcile()`, count sessions with `adapter_type == AdapterType::ClaudeMpm` and pass count to startup struct.

4. **Statusline segment injection** (`src/repl/tui.rs:2280-2288`): After the `TM:` chunk insertion, conditionally push a `"style:claude_mpm"` chunk when `app.claude_mpm_session_count > 0`.

5. **Chunk styler** (`style_status_chunk`): Add a case for `"style:claude_mpm"` prefix — suggested style: dim "style:" prefix + cyan bold "claude_mpm" value.

**Segment position**: The natural location is immediately after `TM: N sessions` in the statusline, mirroring how `local: <model>` follows TM.

---

## Summary Table

| Issue | File(s) | Line(s) | Status | Gap |
|---|---|---|---|---|
| Chat scroll — PageUp/PageDown | `src/repl/tui.rs` | 1354–1361 | Working | Mouse scroll discarded at line 825 |
| Chat scroll — scroll state | `src/repl/tui.rs` | 126–127, 704–711, 3275–3284 | Working | No upper-bound clamp, no scroll indicator |
| claude-mpm detection | `src/adapters/claude_mpm.rs` | 22–32, 70–76 | Broken | Active pane may not be claude-mpm; idle `>` prompt not in brand patterns |
| claude-mpm detection — registry | `src/adapters/registry.rs` | 83–105 | Working | n/a |
| claude-mpm detection — capture | `src/tm/manager.rs` | 247–253, 308–313 | Broken | Captures only active pane, one-shot at reconcile |
| Statusline segments | `src/repl/tui.rs` | 2218–2300, 2388–2433 | Working | No claude_mpm_count field or event |
| Statusline `style:claude_mpm` | `src/repl/tui.rs` | 2252–2288 | Missing | Requires new field + event + render chunk |
