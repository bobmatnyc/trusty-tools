# P1b parity corpus

Goldens for `tests/parity_p1b.rs` (df-T7i P1b): the sensors and wake helpers
`fleet-poll.py` gained after 988152c. Every file here except this README and
`regen_p1b.py` is written by `regen_p1b.py`, which runs the live prototype
(its last change, Architect commit f2741d4, `usage.py` and `input-state.py` pinned at 988152c as
in `../a3/README.md`). Never edit them by hand or regenerate them from Rust.

```bash
python3 -B -I crates/trusty-architect/tests/fixtures/p1b/regen_p1b.py <prototype-dir>
```

## Ruled rows

Where the prototype raises and loses the cycle, the port keeps it:

- `zone.jsonl` `no_boot_time`: no boot time and no stored boot raises
  `KeyError: 'fired'`; the port starts an empty fired list and alerts
  (`warn-None`).
- `../a3/self_ctx_alerts.jsonl` `window_zero`: `ctx_bar.bar_pct` divides by
  the window; the port reports the error and the cycle logs it.
- A `ctx-cycle-alerts.jsonl` line or `selftest-state.json` that is JSON but not
  an object is skipped (the prototype raises `AttributeError`).
