# A3a parity corpus

Goldens for `tests/parity_a3.rs` and the Unicode `\d` table (#9171 A3a).
Every file here except this README and `regen_a3.py`, and
`src/decimal_table.rs`, is written by `regen_a3.py`, which runs the frozen
prototype `fleet-poll.py` (Architect commit 988152c). Do not edit them by hand,
and never regenerate them from the Rust.

`regen_a3.py` imports the prototype read-only (`-B`, `-I`,
`sys.dont_write_bytecode`), points `SUPERVISOR_INBOX_DIR`, `poll.log` and
`events.jsonl` at a system tempdir before any call, replaces the clock with a
fixed `now`, and injects the host probe values. It never touches the live
inbox, `poll-state.json` or `alerts.jsonl`.

## Regenerate

From the repository root, with `<prototype-dir>` the Architect prototype's
`scripts/` directory:

```bash
python3 -B -I crates/trusty-architect/tests/fixtures/a3/regen_a3.py <prototype-dir>
git diff --exit-code crates/trusty-architect/tests/fixtures/a3 crates/trusty-architect/src/decimal_table.rs
```

## Files

| File | Python call | `json.dumps` form |
|---|---|---|
| `corpus_a3.json` | inputs | `indent=1` |
| `list_panes.jsonl` | `list_panes()` over scripted `tmux` stdout | default |
| `band.jsonl` | `builder_band(load15, cores, free, mem, swap, prev)` | default |
| `band_alerts.jsonl` | `band_alerts(state, now, ...)` per step | `ensure_ascii=False` |
| `host.jsonl` | `host_alerts(now, state)` with injected probe values | `ensure_ascii=False` |
| `events.jsonl` | `read_events(state)` per file step | `ensure_ascii=False` |
| `self_ctx_alerts.jsonl` | `self_ctx_alerts(now)`, plus whether it logged | `ensure_ascii=False` |
| `transcript_ctx.jsonl` | `transcript_ctx_pct(pane)` per query | default |
| `alerts_lines.jsonl` | the append in `cycle()`: `json.dumps(a, ensure_ascii=False) + "\n"` | bytes |
| `digits.jsonl` | every code point `re.compile(r"\d")` matches, as ranges | default |

The Python `re` `\d` set depends on the Unicode version of the Python that
runs the live poller (`/opt/homebrew/bin/python3`, 3.14 / Unicode 16.0 at
generation); `src/decimal_table.rs` records both.
