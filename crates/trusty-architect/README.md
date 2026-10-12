# trusty-architect

Leaf library for the trusty-mpm Architect, the one fleet session per user that
watches the other Claude Code sessions in tmux.

Phases A1 and A2 of [#9171](https://github.com/bobmatnyc/trusty-tools/issues/9171)
are here: the crate skeleton with its leaf contracts, and Rust ports of the
poller's pure logic. The poller loop itself is ported in A3 and reached through
`tm architect <verb>`; this crate ships no binary.

## Parity baseline

The ports follow the Architect's live prototype, the scripts the live host runs
from `<prototype-dir>` (the Architect project's `scripts/` directory). The
classifier baseline is the copy inline in `fleet-poll.py`, the poller the host
runs; `fleet-classify.py` is a stale split of it and is not the baseline. The
other baselines are `input-state.py`, `quiet-sessions.py` and `self-ctx.py`,
which `fleet-poll.py` loads unchanged. The copy shipped under
`python/trusty-architect/` is not the baseline. `tests/parity.rs` compares the
Rust output with goldens the prototype Python writes, byte for byte, including
`json.dumps` key order and escaping; `tests/fixtures/parity/README.md` holds the
regeneration command. The prototype's tests that pass against `fleet-poll.py`
are ported by name in `tests/classify_ported.rs`, `tests/quiet_ported.rs` and
`tests/self_ctx_ported.rs`.

## What it provides

- `Tmux`: one tmux command per `run` call. `SystemTmux` spawns `tmux`
  (optionally `-S <socket>`); `FakeTmux` records every call for tests.
- `PaneId`: a tmux `%N` pane id. Targets are always exact ids, never names that
  tmux could prefix-match.
- `InputState`: the input-box states `no-prompt`, `empty`, `suggestion` and
  `typed`.
- `send_guarded`: sends keys only when the box reads `empty` or `suggestion`.
  Any other state returns `SendError::Refused` and sends nothing.
- `input_state_for_send`: the reading to pass `send_guarded`. It fails closed:
  a capture it cannot positively read as an empty or suggestion box (no
  rule-bounded box, an open dialog, a wrapped draft, a stray `❯` row, typed
  text beside a dim span) reads as `no-prompt` or `typed`.
- `classify_input`: `input-state.py`'s single-row classifier, kept exact for
  parity. It does not check that the row is the input box, and it reads a row
  with any dim span as `suggestion`.
- `classify`: `parse_pane`, `evaluate`, `fleet_plan_alerts` and `dedup` from
  `fleet-poll.py`'s inline classifier. Time is passed in as `now`.
- `quiet`: the parked-session list and the test-fixture session-name match.
- `self_ctx`: the Architect's own context from its newest interactive
  transcript. Directories are passed in; nothing reads the environment.
- `to_python_json`: JSON text in Python's `json.dumps` form.
- `poller`: the poller's sensors and `poll-state.json` (A3a), and the wake
  send path (A3b-1). `Waker::wake` types a one-line pointer into the
  Architect's input box only when the pane's live session is `tm-architect`
  and the box reads `empty` or `suggestion`; it presses Enter only when the
  box text equals the pointer, and erases only its own trailing text.
  `dispatch_wake` keeps the pending count until a send succeeds.
- `Clock` (`SystemClock`, `FakeClock`) and `ScriptedTmux`: the time and tmux
  seams the wake tests script.

A `SendReading` is neither `Copy` nor `Clone`: each send needs a fresh
reading of the pane.

```rust
use trusty_architect::{FakeTmux, Keys, PaneId, SendError, input_state_for_send, send_guarded};

let tmux = FakeTmux::new();
let pane = PaneId::parse("%12").expect("a %N pane id");
let reading = input_state_for_send("no input box on this screen");
let refused = send_guarded(&tmux, &pane, reading, Keys::Literal("hi"));
assert!(matches!(refused, Err(SendError::Refused { .. })));
assert!(tmux.calls().is_empty());
```

## Dependency rule

The crate depends only on std, `serde`, `serde_json`, `thiserror`,
`tracing` and `libc`; pattern matching and the SHA-1 fingerprint are written
by hand to stay inside that list. `trusty-mpm` calls
it, so it must never depend on `trusty-mpm` or on any crate that does;
`tests/dependency_boundary.rs` enforces this.

## License

MIT
