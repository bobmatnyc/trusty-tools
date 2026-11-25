# Slash Command Autocomplete — Design Research

**Date**: 2026-05-05
**Scope**: Understand current inline picker, slash command registry, input buffer, and key handling to design slash command autocomplete.

---

## 1. Existing Inline Picker — What It Is and How It Works

**File**: `src/repl/tui.rs`

There are **two separate picker mechanisms**:

### 1a. Modal overlay picker (`app.picker: Option<PickerState>`)

- **State struct**: `PickerState` at line 86
  ```rust
  pub struct PickerState {
      pub items: Vec<String>,
      pub selected: usize,
      pub title: String,
      pub kind: PickerKind,  // Model or Provider
  }
  ```
- **`PickerKind`** (line 73): `Model` or `Provider` — disambiguates which slash command Enter dispatches.
- **Rendered by**: `draw_picker()` at line 2223 — a centered 50%×60% overlay with rounded Cyan border, title, `●` selected indicator, and hint row `↑↓ navigate  Enter select  Esc cancel`.
- **Triggered by**: `ReplEvent::OpenPicker` sent from `src/repl/mod.rs` line 2137/2150 when user submits `/model` or `/provider` with no argument.
- **Key handler**: `handle_picker_key()` at line 1449 — Up/Down with wrap-around, Enter stashes in `app.pending_picker_selection`, Esc clears. Activated in `handle_key()` line 1263 when `app.picker.is_some()` (modal gate — swallows all keys while open).
- **This is NOT the agent picker**. The agent switcher uses the inline list (see 1b).

### 1b. Inline (flat) choice picker (`app.choices: Vec<String>`)

- **Fields** on `ReplApp` (line 283):
  ```rust
  pub choices: Vec<String>,       // list items
  pub choice_cursor: usize,       // highlighted index (line 284)
  pub choices_context: Option<String>,  // e.g. "switch" (line 286)
  ```
- **`picker_height`** (line 1506): computed as `choices.len().min(8)` — this is the variable being asked about. It reserves rows in the layout.
- **`inline_picker_area`** (line 1599): a layout chunk inserted between the bottom separator and the statusline when `picker_height > 0`.
- **Rendered by**: `draw_inline_choice_picker()` at line 2160 — borderless flat list. Selected row: `▶ ` cyan/bold prefix; others: `  ` (two spaces) + dim. Scrolls with a sliding window when choices > visible rows.
- **Triggered by**: `ReplEvent::SetChoices` sent from `src/repl/mod.rs` line 2116 — currently only fired for `/switch` (no arg), populating `["ctrl", "Izzie", "CTO Assistant"]` with `context = Some("switch")`.
- **Key handler** (inside `handle_key()`, line 1270): Up/Down navigate, Enter commits, Esc dismisses. When `context == Some("switch")`, Enter synthesizes `/switch <selected>` via `app.pending_submit`; otherwise it inserts into `input_buf`.
- **This is the agent/persona picker** — the one showing "ctrl ▶ Izzie CTO Assistant".

---

## 2. Slash Command Registry

**File**: `src/repl/mod.rs`

### `try_handle_slash()` — line 594

All slash commands handled via a `match cmd` block. Full command list (from `write_help()` at line 1896):

| Command | Handler line | Notes |
|---|---|---|
| `/help` | 607 | Calls `write_help()` |
| `/exit`, `/quit` | 611 | Returns `Ok(false)` to stop REPL |
| `/clear` | 615 | Clears history, persona, overrides |
| `/provider [name]` | 626 | Sets credential routing |
| `/model [id]` | 634 | Sets model for session |
| `/agent [name]` | 638 | Switch persona or list agents |
| `/switch [name]` | 642 | Flip front-end voice (ctrl/Izzie/CTO) |
| `/agents` | 653 | List available agents |
| `/skills` | 657 | List available skills |
| `/memories [query]` | 661 | Search memory store |
| `/status` | 665 | Controller liveness |
| `/session` | 671 | Session ID + socket |
| `/connect`, `/cd` | 676 | Switch project |
| `/version` | 680 | Build info |
| `/projects` | 695 | List projects |
| `/log [N]` | 699 | Tail perf log |
| `/run <file>` | 708 | Forward task from file |
| `/history [N]` | 745 | REPL input history |
| `/telegram [cmd]` | 754 | Telegram bot gateway |
| `/logs` | 758 | Tail chat log entries |
| `/local [on|off|test]` | 766 | Ollama fast-path control |
| `/tm <subcmd>` | 772 | Tmux session manager |
| (other) | 781 | "unknown command: X (type /help)" |

**No `SLASH_COMMANDS` const exists** — the commands are embedded only as match arms in `try_handle_slash`. The authoritative human-readable list is the `write_help()` function at line 1896.

### Pre-intercept in `ReplBridge` — `src/repl/mod.rs` line ~2100

Before `try_handle_slash` is called, `/switch` (no arg), `/model` (no arg), and `/provider` (no arg) are intercepted by `ReplBridge::handle_line()` to fire picker events. These never reach `try_handle_slash`.

---

## 3. Input Handling

**File**: `src/repl/tui.rs`

### Input buffer

- **Field**: `app.input_buf: String` at line 123
- **Cursor**: `app.cursor_pos: usize` (byte offset) at line 124
- **Mutated via helpers** on `ReplApp`: `insert_char()` line 643, `backspace()` line 654, `cursor_left()`, `cursor_right()`, `set_input()` line 638, `take_input()` line 719 (drains buffer on Enter).

### Key event handling

- **Entry point**: `handle_key(app, key)` at line 1260, called from `process_event()` at line 1004.
- **Dispatch priority** (highest to lowest):
  1. Modal picker gate (line 1263): if `app.picker.is_some()`, delegate to `handle_picker_key()` — all keys consumed.
  2. Inline choice picker (line 1270): Up/Down/Enter/Esc when `!app.choices.is_empty()` — other keys fall through.
  3. Ctrl combos (line 1326): `^C` clear, `^D` quit-if-empty, `^A` start, `^E` paste-bash/end, `^U` clear line.
  4. Normal editing (line 1382): Enter submits, Char inserts, Backspace deletes, Left/Right/Home/End move cursor, Up recalls last prompt, Down history-next, PageUp/PageDown scroll.

### Tab key

**Tab is not handled.** `KeyCode::Tab` does not appear anywhere in `handle_key()` or `handle_picker_key()`. It falls through to the `_ => None` arm at line 1434 and is silently ignored.

---

## 4. Design Notes for Slash Command Autocomplete

### What exists to reuse

- The **inline choice picker** (`app.choices` + `draw_inline_choice_picker`) is already the right UX pattern — inline, below input, keyboard-navigable, dismissable with Esc. It is currently used only for `/switch`.
- The input buffer (`app.input_buf`) is directly accessible and mutated per-keypress in `handle_key()`.
- The `ReplEvent::SetChoices { items, context }` event already supports filtered lists.

### What needs to be added

1. **In `handle_key()` (line 1384, `KeyCode::Char(c)` arm)**: After `app.insert_char(c)`, check if `input_buf` starts with `/` and compute filtered completions. If completions exist and differ from current choices, emit a `SetChoices` event — but `handle_key` only has `&mut ReplApp`, no channel. Options:
   - Add a `pending_set_choices: Option<(Vec<String>, Option<String>)>` field on `ReplApp` (analogous to `pending_submit`) that the event loop drains after `handle_key` returns.
   - Or compute and set `app.choices` directly inside `handle_key` (simpler — no channel needed since `SetChoices` just writes to the same fields).

2. **Slash command list**: Extract a `const SLASH_COMMANDS: &[(&str, &str)]` table (name, description) from the `write_help()` strings. This is the single source of truth for autocomplete items.

3. **Filtering logic**: When `input_buf` starts with `/` and has no space yet (i.e., still typing the command name), filter `SLASH_COMMANDS` by prefix match on the typed fragment and populate `app.choices`. Clear choices when the buffer no longer starts with `/`, contains a space (command fully typed), or is empty.

4. **Tab completion**: Handle `KeyCode::Tab` in `handle_key()` — if choices are non-empty, complete `input_buf` to the selected choice (append the full command name + space). If exactly one match, complete immediately. If multiple, Tab cycles or opens the picker.

5. **Enter handling in choice picker**: The existing inline picker's Enter handler (line 1282) inserts into `input_buf`. For slash autocomplete the `choices_context` would be `None` (default path), so Enter inserts the selected `/command` name into the buffer — user can then add arguments and submit.

### Key design decision

The existing inline picker was designed for LLM-generated lists (detected via `detect_choices()`) and for `/switch`. Reusing it for slash autocomplete is the right approach — same rendering, same navigation — but the population trigger moves from "LLM response parsed" to "user types `/`". This can be done entirely within `handle_key()` by writing to `app.choices` directly (no event loop needed).

---

## File Locations Summary

| Topic | File | Lines |
|---|---|---|
| `PickerState` struct | `src/repl/tui.rs` | 86–91 |
| `PickerKind` enum | `src/repl/tui.rs` | 73–76 |
| `ReplApp.choices` field | `src/repl/tui.rs` | 283–290 |
| `ReplApp.input_buf` field | `src/repl/tui.rs` | 123–124 |
| `picker_height` computation | `src/repl/tui.rs` | 1506–1510 |
| `inline_picker_area` allocation | `src/repl/tui.rs` | 1599–1604 |
| `draw_inline_choice_picker()` | `src/repl/tui.rs` | 2160–2222 |
| `draw_picker()` (modal overlay) | `src/repl/tui.rs` | 2223–2283 |
| `handle_key()` | `src/repl/tui.rs` | 1260–1436 |
| `handle_picker_key()` | `src/repl/tui.rs` | 1449–~1490 |
| `try_handle_slash()` | `src/repl/mod.rs` | 594–788 |
| `write_help()` (command list) | `src/repl/mod.rs` | 1896–1934 |
| `/switch` SetChoices trigger | `src/repl/mod.rs` | 2104–2120 |
| `/model`, `/provider` OpenPicker trigger | `src/repl/mod.rs` | 2122–2156 |
