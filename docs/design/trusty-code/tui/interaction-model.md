# Interaction Model — Claude Code TUI

**Status:** Informative (design analysis)
**Last-updated:** 2026-09-06
**Sources:** `code.claude.com/docs/en/interactive-mode` (primary — full
shortcut tables quoted verbatim below), `permission-modes`, `permissions`,
`checkpointing`, `fullscreen`.

## Keyboard map — general controls

| Shortcut | Description | Context |
|---|---|---|
| `Ctrl+C` | Interrupt, or clear input | First press (idle) clears prompt; second exits Claude Code |
| `Ctrl+X Ctrl+K` | Stop all background subagents, disable artifact auto-replies for the rest of the session | press twice within 3s to confirm |
| `Ctrl+D` | Exit session | First press shows confirmation hint, second within 800ms exits; with text in prompt, deletes char after cursor instead |
| `Ctrl+G` / `Ctrl+X Ctrl+E` | Open prompt in `$EDITOR` | |
| `Ctrl+L` | Redraw/clear screen | Recovers a garbled display; in fullscreen also clears and lets you scroll up |
| `Ctrl+O` | Toggle transcript viewer | Shows tool usage, timestamp, model per message |
| `Ctrl+R` | Reverse search history | |
| `Ctrl+V` / `Cmd+V` (iTerm2) / `Alt+V` (Win/WSL) | Paste image from clipboard | inserts `[Image #N]` chip |
| `Ctrl+B` | Background running Bash/agent task | tmux users press twice |
| `Ctrl+T` | Toggle task checklist | not the same as `/tasks` |
| `Ctrl+S` | Stash/restore prompt text | |
| `Ctrl+Z` | Suspend (Unix only) | |
| `Left/Right` | Cycle dialog tabs | permission dialogs, menus |
| `Tab` | Accept autocomplete, or add a comment to a permission answer | |
| `Up/Down`, `Ctrl+P`/`Ctrl+N` | Move cursor within multi-line input, else navigate history | queued messages: `Up` from first row takes them back |
| `Esc` | Interrupt turn, or close dialog / decline permission (= No, no comment) | keeps queued messages, sends them next |
| `Esc Esc` | Clear input draft (saved to history), or open rewind menu when input is empty | |
| `Shift+Tab` (or `Alt+M` on some Windows terminals) | Cycle permission modes: default → acceptEdits → plan → [bypassPermissions] → [auto] → default | |
| `Option+P`/`Alt+P` | Switch model | |
| `Option+T`/`Alt+T` | Toggle extended thinking | |
| `Option+O`/`Alt+O` | Toggle fast mode | |

## Text editing

| Shortcut | Action |
|---|---|
| `Ctrl+A` / `Ctrl+E` | Start / end of current logical line |
| `Ctrl+K` | Delete to end of line (stores for paste) |
| `Ctrl+U` | Delete cursor-to-line-start (stores for paste); repeat clears multiline |
| `Ctrl+W` | Delete back to previous whitespace (whole path/`--flag=value` in one press) |
| `Ctrl+Y` | Paste last deleted text; `Alt+Y` after that cycles paste history |
| `Alt+B`/`Alt+F` | Word back/forward (letters+digits only; punctuation is a boundary) |
| `Alt+D` | Delete to end of word |
| `Ctrl+_` / `Ctrl+Shift+-` | Undo last input edit |

`Ctrl+W` treats punctuation as part of the deletion (removes a whole path in
one press); the `Alt+B/F/D` family treats punctuation as a word boundary. This
distinction is a real usability asymmetry worth preserving deliberately.

## Multi-line input

| Method | Shortcut |
|---|---|
| Quick escape | `\` + `Enter` |
| Option key | `Option+Enter` (macOS, after enabling Option-as-Meta) |
| Shift+Enter | native in iTerm2/WezTerm/Ghostty/Kitty/Warp/Terminal.app/Windows Terminal |
| Control sequence | `Ctrl+J` (works everywhere, no config) |
| Paste mode | paste code blocks/logs directly |

## Quick commands

| Prefix | Meaning |
|---|---|
| `/` at start | command or skill — autocomplete filters as you type |
| `!` at start | shell mode: run directly, add output+response to context, `Ctrl+B` backgrounds, `Tab` completes from `!`-history, live path autocomplete on tokens containing `/` |
| `@` | file-path mention autocomplete; also suggests other live sessions on this machine (cross-session messaging) |
| `:` | emoji shortcode — `:name:` inserts, 2+ chars opens suggestions |
| `?` on empty input | toggles shortcut help panel |

## Vim mode

Enabled via `/config` → Editor mode. Mode persists across `Ctrl+O` toggles and
panel open/close.

**Mode switching:** `Esc`/`Ctrl+[` → NORMAL; `i`/`I`/`a`/`A`/`o`/`O` → INSERT
variants; `v`/`V` → VISUAL (char/line-wise). `vimInsertModeRemaps` (e.g. `jj`
→ Escape) is settable in user/managed settings only — project settings cannot
remap keystrokes.

**NORMAL navigation:** `h j k l`, `Space`, `w e b`, `0 $ ^`, `gg G`,
`f{c} F{c} t{c} T{c}`, `; ,`, `/` (reverse history search).
**NORMAL editing:** `x`, `dd D dw de db`, `df{c} dt{c}`, `cc C cw ce cb`,
`s S` (v2.1.211+), `yy Y yw ye yb`, `p P`, `>> <<`, `J`, `u`, `.`.
**Text objects:** `iw/aw`, `iW/aW`, `i"/a"`, `i'/a'`, `i(/a(`, `i[/a[`,
`i{/a{` — operate with `d`/`c`/`y`.
**VISUAL:** `d/x` delete, `y` yank, `c/s` change, `p` replace, `r{c}`
replace-each, `~/u/U` case, `>/<` indent, `J` join, `o` swap ends. Block-wise
`Ctrl+V` visual is **not supported**.

At cursor extremes in NORMAL mode, `j/k` and arrows fall through to command
history navigation instead of failing silently.

## Command history

Stored per working directory. `/clear` starts a new session for recall
purposes (new session's prompts list first). Duplicate consecutive submits
collapse to one history entry. `Ctrl+R` reverse-search: classic renderer
searches inline across all projects; fullscreen opens a dialog with `Ctrl+S`
cycling scope (session / project / all projects).

## Interrupt semantics

- **`Esc`** stops the current response/tool call mid-turn; work done so far
  is kept; queued messages (see below) are sent next.
- **`Ctrl+C`** on an idle prompt clears input (first press) then exits
  (second press); on a running turn it interrupts, same as `Esc`, per the
  general-controls table.
- **`Ctrl+X Ctrl+K`** is the heavier interrupt: stops every background
  subagent and disables artifact auto-replies (double-press-to-confirm).

## Queueing a message while a turn runs

Typing a message and pressing `Enter` mid-turn **queues** it rather than
interrupting — queued entries list above the input box. Delivery depends on
kind:

- **Plain messages:** delivered to Claude as soon as in-flight tool calls
  finish (same turn) if still mid-turn, or as the very next turn (oldest
  first) once the turn ends; the rest keep queueing behind the same rule.
- **Commands / shell commands:** held until the turn ends, then run one at a
  time — except a short list Claude Code runs immediately on submit
  (`/status`, `/model`, `/effort`, `/fast`).
- **Take-back:** `Up` from the first input row (empty prompt) pulls every
  queued item back into the input box, one per line, ahead of anything typed;
  editing and re-pressing `Enter` re-queues as one entry.

`Esc` interrupts the turn and sends what's queued immediately instead of
waiting for the turn to end naturally.

## Rewind and checkpoints

`/rewind` or `Esc Esc` on empty input opens the rewind menu (full layout in
`screen-inventory.md` §20). Checkpoints:

- Captured automatically before every user prompt; the 100 most recent are
  kept per session, with each file's *first* snapshot retained even after its
  checkpoint ages out (VS Code diff baseline).
- Checkpoints persist with the conversation — `/rewind` works after
  `--resume`.
- **Not tracked:** bash-command file modifications (`rm`, `mv`, `cp` via
  shell), most subagent edits (exception: a foreground-forked skill),
  external/concurrent-session edits, symlinked/hard-linked paths (skipped
  with a warning, current contents kept).
- Rewind is explicitly **not a substitute for version control**.

## Paste handling

- Plain paste inserts as multi-line text (see Multi-line input above).
- Image paste (`Ctrl+V`/`Cmd+V`/`Alt+V`) inserts a positional `[Image #N]`
  chip.
- A recalled history entry that had pasted content resends the full pasted
  content on resubmit (unless it has since been retention-cleaned, in which
  case the literal `[Pasted text #N]` marker is not resent — interactive-mode
  doc, "Command history").

## File @-mention and slash-command autocomplete

- `@` triggers file-path autocomplete; in fullscreen the suggestion list also
  responds to the mouse (hover-highlight, click-to-accept).
- `/` triggers command/skill autocomplete; the menu lists built-ins, bundled
  and user skills, and plugin/MCP-contributed commands; a few built-ins are
  intentionally hidden from the menu and only run when typed in full.
- Skill `argument-hint` frontmatter drives the hinted-argument display during
  `/`-autocomplete.

## Terminal setup and platform notes

- macOS Option/Alt-key shortcuts (`Alt+B/F/D/Y/P`) require configuring Option
  as Meta in the terminal profile.
- Fullscreen rendering requires the alt-screen buffer (like `vim`/`htop`);
  incompatible with iTerm2's `tmux -CC` integration mode; needs `set -g mouse
  on` in `~/.tmux.conf` for wheel scrolling under plain tmux.
