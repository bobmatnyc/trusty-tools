# trusty-architect

Leaf library for the trusty-mpm Architect, the one fleet session per user that
watches the other Claude Code sessions in tmux.

This is phase A1 of [#9171](https://github.com/bobmatnyc/trusty-tools/issues/9171):
the crate skeleton and its leaf contracts. The Python poller logic is ported in
later phases and reached through `tm architect <verb>`; this crate ships no
binary.

## What it provides

- `Tmux`: one tmux command per `run` call. `SystemTmux` spawns `tmux`
  (optionally `-S <socket>`); `FakeTmux` records every call for tests.
- `PaneId`: a tmux `%N` pane id. Targets are always exact ids, never names that
  tmux could prefix-match.
- `InputState`: the input-box states `no-prompt`, `empty`, `suggestion` and
  `typed`.
- `send_guarded`: sends keys only when the box reads `empty` or `suggestion`.
  Any other state returns `SendError::Refused` and sends nothing.

```rust
use trusty_architect::{FakeTmux, InputState, PaneId, SendError, send_guarded};

let tmux = FakeTmux::new();
let pane = PaneId::parse("%12").expect("a %N pane id");
let refused = send_guarded(&tmux, &pane, InputState::Typed, &["hi", "Enter"]);
assert!(matches!(refused, Err(SendError::Refused { .. })));
assert!(tmux.calls().is_empty());
```

## Dependency rule

The crate depends only on std, `thiserror` and `tracing`. `trusty-mpm` calls
it, so it must never depend on `trusty-mpm` or on any crate that does;
`tests/dependency_boundary.rs` enforces this.

## License

MIT
