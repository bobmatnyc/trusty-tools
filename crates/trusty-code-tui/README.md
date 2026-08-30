# trusty-code-tui

The engine-agnostic terminal-UI seam shared by trusty-code's `tcode tui` and
trusty-agents' tagent REPL.

Both products need the same ratatui event loop, widgets, and line editing.
Forking one for the other would duplicate that code and let the two drift, so
it lives here once and each product supplies a thin `TuiEngine` adapter.

## What's here

| Module | Surface |
|---|---|
| `engine` | `TuiEngine` — the trait a product implements to drive the shared loop |
| `event` | `ReplEvent` — the event vocabulary the engine and the loop speak; no terminal-library dependency |
| `model` | `StatuslineSegment`, `PickerItem`/`PickerRequest`, `CommandDescriptor` — engine-supplied data, so no product constant is hardcoded here |
| `terminal` | `TerminalGuard` — panic-safe raw-mode/alternate-screen entry and exit |
| `run` | The render/event loop, generic over `TuiEngine` |
| `keys` | `crossterm` → `KeyInput` translation |
| `app`, `widgets`, `render`, `layout` | The reducer model, the widgets that draw it, markdown rendering, and frame composition |
| `commands` | The slash-command parser: `/help`, `/clear`, `/quit`, `/exit` are client-side built-ins; every other command forwards verbatim to the engine |

## Dependency direction

`trusty-code` and `trusty-agents` depend on `trusty-code-tui`;
`trusty-code-tui` depends on neither. Its public API never references a
product-specific type.

`ratatui` 0.30 / `crossterm` 0.29 are confined to `terminal`, `run`, and
`keys`. They diverge from the rest of the workspace's 0.29/0.28 pin during the
tagent migration window — see the comment in `Cargo.toml`.

## Design

DOC-50 (`docs/specs/DOC-50-tcode-tui-claude-code-clone.md`) §2.2 and §5, epic
[#3411](https://github.com/bobmatnyc/trusty-tools/issues/3411).

## License

MIT — see [LICENSE](LICENSE).
