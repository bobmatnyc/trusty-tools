Fixed

- `tm ls` no longer shows dead session records in its default view (closes [#4994](https://github.com/bobmatnyc/trusty-tools/issues/4994))
  - a record the listing-time sweep confirms dead — no tmux pane, no workspace directory left on disk — was rendered `[dead]` but still listed, because `is_live_session_state` consulted lifecycle state alone
  - visibility keys off that sweep's own verdict, never the raw `unresumable` wire flag: the flag is computed from a record's persisted state and can ship alongside `state: "active"` for a session whose pane is running, which the sweep then refuses to reap. Hidden now implies reapable
  - the records are untouched: `tm ls --all` still lists every one, and the remedy is unchanged (`tm sessions delete <id> --force`)
  - the default-view filter runs AFTER the sweep, so it still sees the records it reaps
  - the "N more dead records pending confirmation" line says where those rows went, and only where they were actually hidden — not on `--json` or `--all`, which print them
  - that line now also counts records whose decommission was attempted and failed. They stay hidden either way, so leaving them out meant a sweep where every attempt failed printed nothing at all and the rows vanished with no explanation
