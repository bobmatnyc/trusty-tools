Added
- `tm architect poll|classify|input-state|quiet|self-ctx`, the Rust stand-ins for the Architect's `fleet-poll.py`, `fleet-classify.py`, `input-state.py`, `quiet-sessions.py` and `self-ctx.py`, with the same arguments and stdout; each exits 1 when it cannot read its input, reach tmux or parse state, and 2 on a bad argument ([#9822](https://github.com/bobmatnyc/trusty-tools/issues/9822))
- The seeded `tm-fleet-check` and `tm-context-refresh` skills classify the input box with `tm architect input-state` ([#9822](https://github.com/bobmatnyc/trusty-tools/issues/9822))
