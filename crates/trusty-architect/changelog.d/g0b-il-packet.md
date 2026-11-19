Added
- `ledger::il`: the IL packet parser and checker ported from `ilio.py` (last ```` ```il ```` fence, required fields, field order, 240-character lines, `pct`, ledger ids, `new:` kind, `act:` parts and datg's act limits), and `il::apply`, which writes a checked packet to a session's `datg` store with the bytes `datg.py` writes. A replayed packet is a no-op (G0b).
- `StorePaths::at`, `Writer::batch_at` and `writer::seen`, so a batch can target the `.trusty-mpm/datg/<session>` store (G0b).
