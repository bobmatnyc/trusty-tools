Added
- `ledger::il`: the IL packet parser and checker ported from `ilio.py` (last ```` ```il ```` fence, required fields, field order, 240-character lines, `pct`, ledger ids, BADID look-alike ids, the `remind:` form, a `pct` unchanged while `done:` claims progress, `new:` kind, `act:` parts and datg's act limits), and `il::apply`, which writes a checked packet to a session's `datg` store with the bytes `datg.py` writes. A replayed packet is a no-op (G0b).
- `il::route` sends a block the Rust writer does not fully cover to `ilio.py apply`, and `il::mark_applied` records an ilio-applied packet so its replay is a no-op (G0b).
- `StorePaths::at`, `Writer::batch_at` and `writer::seen`, so a batch can target the `.trusty-mpm/datg/<session>` store (G0b).
- The IL checker refuses `ilio.py`'s own `SECRET` shapes (`password|secret|token|api_key` assignments, `sk-` keys, private-key headers, any case) as well as the injected scan (G0b).
- An `il::apply` whose `act` lines fail after the batch commits is no longer a replay: applying the packet again appends the acts once, and the packet replays only after they land (G0b).
- `il::hold`, `il::mark_pending`, `il::clear_pending` and `il::pending_unknown`: a per-store apply lock and pending markers, so one packet runs `ilio.py apply` at most once (G0b).
- `il::apply` and `il::route` write only what the caller hands them; the `tm hook` caller hands them the session's own turn only, never a handback or inbound message (see G0b IL authority (owner decision pending)).
