# TUI Key Handling and Popup Selector Research

**Date:** 2026-05-02  
**Purpose:** Understand the ratatui event loop to plan adding a popup selector overlay (e.g. model picker).

---

## 1. Key Event Flow: Keypress → TUI Update

### Thread architecture

```
OS thread (blocking crossterm::event::read)
    |── Ok(CtEvent::Key(k))   → key_tx.send(ReplEvent::Key(k))
    |── Ok(CtEvent::Resize)   → key_tx.send(ReplEvent::Resize(c,r))
    └── Err(_)                → break (exits thread)

tokio event_loop (async)
    |── tick every 250ms      → terminal.draw(|f| draw(f, &snap))
    └── rx.recv()             → process_event(ev, &app, &tx, &handler).await
                               └── terminal.draw(|f| draw(f, &snap))
```

### `process_event` dispatching (src/repl/tui.rs:535)

```
ReplEvent::Key(k)
    → handle_key(&mut app, k)         // SYNCHRONOUS, holds Mutex lock
      returns Option<String> (submitted line)
    → if Some(line):
        app.push_user(&line)
        app.thinking = true
        tokio::spawn → handler.handle_input(line, tx)
                     → emits LlmResponse / StatusMessage / LabelChanged / etc.
```

### `handle_key` function (src/repl/tui.rs:639–716)

This is the single gate for ALL key input. It is a plain function that takes `&mut ReplApp` and `KeyEvent` and returns `Option<String>`.

**Ctrl combos handled:**
- `Ctrl-C` → clear input_buf, return None
- `Ctrl-D` → set app.quit = true (if input empty)
- `Ctrl-A` → cursor_pos = 0
- `Ctrl-E` → cursor_pos = input_buf.len()
- `Ctrl-U` → clear input_buf

**Regular keys handled:**
| Key | Action |
|-----|--------|
| Enter | `app.take_input()` → returns Some(line), clears buffer |
| Char(c) | `app.insert_char(c)` |
| Backspace | `app.backspace()` |
| Left/Right | cursor movement |
| Home/End | cursor to start/end |
| Up | `app.history_prev()` |
| Down | `app.history_next()` |
| PageUp | `app.scroll(-10)` |
| PageDown | `app.scroll(10)` |
| Esc | falls through to `_ => None` (NOT handled) |

**Critical: Esc is currently a no-op.** This is the natural key for dismissing a popup.

---

## 2. All ReplEvent Variants (src/repl/tui.rs:160–202)

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
    Submit(String),               // defined but not actively used (handled inline)
    TokenUpdate { prompt: u64, completion: u64 },
    TokenReset,
    StatuslineUpdate { model: String, provider: String },
}
```

**New variants needed for picker:**
```rust
// Open the model picker popup
OpenModelPicker,
// User confirmed a selection from the picker  
ModelPickerSelected(String),
// User dismissed the picker without choosing
ModelPickerDismissed,
```

---

## 3. ReplApp State Fields (src/repl/tui.rs:88–151)

**Input/editing fields:**
- `input_buf: String` — current input line being edited
- `cursor_pos: usize` — byte offset within input_buf
- `history: Vec<String>` — in-memory command history
- `history_idx: Option<usize>` — history navigation index
- `saved_input: Option<String>` — saved input while navigating history
- `thinking: bool` — whether LLM is busy
- `thinking_lines: Vec<String>` — curated thinking step lines
- `scroll_offset: usize` — lines scrolled up from bottom
- `quit: bool` — exit signal

**No existing modal/popup state.** The ReplApp has no `mode` field or overlay state.

---

## 4. Existing Popup Infrastructure

**None exists.** Search confirmed:
- No `ListState`, `List` widget, `centered_rect`, `Clear` widget, or popup/overlay pattern anywhere in `src/repl/`
- The debugger TUI (`src/debugger/tui.rs`) uses a `Focus` enum to switch keyboard routing between panels — this is the closest existing pattern to modal mode

**The debugger's `Focus` enum approach is the right model for picker mode:**
```rust
// debugger uses:
pub enum Focus { Left, Right, Input }
// We'd add a picker "mode" to ReplApp similarly
```

---

## 5. How Slash Commands Return Content to TUI

`try_handle_slash` (src/repl/mod.rs:340–500) returns `Option<Result<(bool, String)>>`:
- `None` → not a slash command, caller falls through to LLM
- `Some(Ok((true, output)))` → continue REPL, display `output` as response
- `Some(Ok((false, output)))` → quit after showing output
- `Some(Err(e))` → show error

`ReplBridge::handle_input` (src/repl/mod.rs:1443–1568) handles the bridge:
1. Checks natural-language agent switch
2. Checks slash commands via `try_handle_slash`
3. On slash command result: sends `ReplEvent::LlmResponse { text: output }` through `tx`
4. Also sends `LabelChanged`, `AgentScopeChanged`, `StatuslineUpdate` side-effects

**For the model picker flow**, the `/model` command currently works like:
```
/model <id> → handle_model_command_into → writes to out String
           → ReplBridge sends ReplEvent::LlmResponse + StatuslineUpdate
```

The picker would intercept `/model` (with no arg, or a new trigger like `/model pick`) and instead of running immediately, emit an `OpenModelPicker` event that sets popup state — deferring the actual model change until selection.

---

## 6. Available Models: Where to Get the List

**Option A: Hardcoded known-good list** — simplest, no I/O
```rust
const KNOWN_MODELS: &[&str] = &[
    "claude-haiku-4-5",
    "claude-sonnet-4-6",
    "claude-opus-4-5",
    "gpt-4o",
    "gpt-4o-mini",
    // ...
];
```

**Option B: Read all agent TOMLs** — the codebase already has `discover_agent_names` in `src/repl/mod.rs:1178`. The model field is parsed by `resolve_active_model()` (src/repl/mod.rs:300) with this pattern:
```rust
// Reads ctrl.toml and pm.toml looking for lines starting with "model"
for line in s.lines() {
    let l = line.trim();
    if let Some(rest) = l.strip_prefix("model") {
        if let Some(eq) = rest.find('=') {
            let val = rest[eq + 1..].trim();
            return val.trim_matches('"').to_string();
        }
    }
}
```
Could extend to scan ALL agent TOMLs and collect unique model strings.

**Option C: Ollama list** — already fetched in `probe_ollama()` (src/repl/mod.rs:1388). When `/provider local` is active, the picker should show the ollama models from that last probe. These could be stored in `ReplApp.provider_override` context or a new field.

**Recommended: A + C combination** — hardcoded baseline list always shown; if `provider_override == "local"`, show ollama models instead.

---

## 7. How to Add Modal Mode: Recommended Approach

### New state in ReplApp

```rust
// In ReplApp:
pub picker: Option<PickerState>,

// New struct (can live in tui.rs):
pub struct PickerState {
    pub items: Vec<String>,
    pub list_state: ratatui::widgets::ListState,
    pub title: String,
}
```

`PickerState` carries its own `ListState` (which tracks selection index for ratatui's `List` widget). No separate struct file needed — fits cleanly in `tui.rs`.

### Modified `handle_key`

```rust
fn handle_key(app: &mut ReplApp, key: KeyEvent) -> Option<String> {
    // MODAL GATE: if picker is open, all keys route to picker
    if app.picker.is_some() {
        return handle_picker_key(app, key);
    }
    // ... existing logic unchanged ...
}

fn handle_picker_key(app: &mut ReplApp, key: KeyEvent) -> Option<String> {
    match key.code {
        KeyCode::Up => { /* move selection up */ }
        KeyCode::Down => { /* move selection down */ }
        KeyCode::Enter => {
            let selected = /* get selected item */;
            app.picker = None;
            // Return None but the caller (process_event) needs to apply the selection.
            // Options:
            //   1. Return a sentinel string like "__picker_select::<model>"
            //   2. Mutate app.pending_selection: Option<String>
            //   3. Add a new ReplEvent variant that process_event checks after handle_key
        }
        KeyCode::Esc => {
            app.picker = None;
        }
        _ => {}
    }
    None
}
```

**The cleanest mechanism for communicating picker selection back to the handler** is option 2: add `pending_selection: Option<String>` to `ReplApp`, which `process_event` checks after `handle_key` returns:

```rust
// In process_event, after handle_key:
if let Some(selected_model) = app.pending_selection.take() {
    // apply the model override and emit StatuslineUpdate
    // send StatusMessage "Model set to: <selected>"
}
```

This avoids new ReplEvent variants while keeping `handle_key` synchronous and mutation-only.

### Popup rendering in `draw`

```rust
pub fn draw(f: &mut ratatui::Frame, app: &ReplApp) {
    // ... existing banner/chat/input/statusline draw ...

    // Popup overlay — rendered LAST so it draws on top
    if let Some(picker) = &app.picker {
        draw_model_picker(f, picker, f.area());
    }
}

fn draw_model_picker(f: &mut ratatui::Frame, picker: &PickerState, area: Rect) {
    use ratatui::widgets::{Clear, List, ListItem};
    
    let popup_area = centered_rect(60, 50, area);  // 60% width, 50% height
    f.render_widget(Clear, popup_area);             // erase background
    
    let items: Vec<ListItem> = picker.items.iter()
        .map(|m| ListItem::new(m.as_str()))
        .collect();
    
    let list = List::new(items)
        .block(Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(picker.title.as_str()))
        .highlight_style(Style::default()
            .bg(Color::Cyan)
            .fg(Color::Black)
            .add_modifier(Modifier::BOLD))
        .highlight_symbol("> ");
    
    f.render_stateful_widget(list, popup_area, &mut picker.list_state.clone());
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}
```

**Note:** `ListState` must be `mut` for `render_stateful_widget`. Either store `picker` as `Option<PickerState>` directly in `ReplApp` (which is already `Clone`, but `ListState` is `Clone` too — confirmed in ratatui 0.29), or accept a `&mut PickerState` in the draw function. The cleanest approach is `draw_model_picker(f, &mut app.picker, area)` taking `&mut ReplApp` — but `draw` currently takes `&ReplApp`. This means `PickerState` must store `list_state` as a `Cell<ListState>` or `RefCell<ListState>`, OR `draw` must take `&mut ReplApp`.

**Simplest resolution:** Change `draw(f, &ReplApp)` to `draw(f, &mut ReplApp)` — there are no unit tests that call `draw` directly, so this is a safe signature change.

---

## 8. Integration with `/model` Slash Command

The `/model` flow in `ReplBridge::handle_input` (mod.rs:1512–1549) currently calls `try_handle_slash` which calls `handle_model_command_into`. To open the picker instead:

```rust
// In ReplBridge::handle_input, before the existing slash dispatch:
if trimmed == "/model" {
    // No arg — open picker instead of showing usage text
    let items = build_model_list(&repl);  // hardcoded + ollama if local
    // Send a new event... but handle_input doesn't have direct app access.
    // It only has `tx: UnboundedSender<ReplEvent>`.
    let _ = tx.send(ReplEvent::OpenModelPicker { items });
    return Ok(true);
}
```

This requires the `OpenModelPicker` event variant after all, since `handle_input` cannot mutate `ReplApp` directly (it only has the `tx` channel). `process_event` handles this event by setting `app.picker = Some(PickerState { items, ... })`.

The full selection flow:
1. User types `/model` (no arg) → Enter
2. `ReplBridge::handle_input` sends `ReplEvent::OpenModelPicker { items }`
3. `process_event` sets `app.picker = Some(PickerState { ... })`
4. `draw` renders the overlay on next tick
5. All key events route through `handle_picker_key` (modal gate)
6. Enter: sets `app.pending_selection = Some(selected)`, clears `app.picker`
7. `process_event` checks `pending_selection`, applies model override, sends `StatuslineUpdate` + `StatusMessage`
8. Esc: clears `app.picker`, sends `StatusMessage("Model picker dismissed")`

---

## Summary

| Question | Answer |
|----------|--------|
| Central key handler | `handle_key(app, key)` in tui.rs:639 — single synchronous function |
| Where input_buf is filled | `app.insert_char(c)` on `KeyCode::Char` |
| Where Enter is handled | `app.take_input()` returns the line; process_event spawns handler task |
| Where Up/Down are handled | `app.history_prev()` / `app.history_next()` in handle_key |
| Esc handling | Not handled (falls to `_ => None`) — free to use for picker dismiss |
| Any existing popup code | None in repl/ — `Clear` widget used only in status_bar.rs (crossterm terminal clearing, not ratatui) |
| ReplEvent variants count | 12 variants |
| Picker state approach | Add `picker: Option<PickerState>` to ReplApp + modal gate at top of handle_key |
| Model list source | Hardcoded baseline + ollama list when provider_override == "local" |
| Slash command return path | `try_handle_slash` → `ReplBridge` sends `LlmResponse { text }` via `tx` |
| Picker trigger mechanism | New `ReplEvent::OpenModelPicker { items }` sent from handle_input via tx |
