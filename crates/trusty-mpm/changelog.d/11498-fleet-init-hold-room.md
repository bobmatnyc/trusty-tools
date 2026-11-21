Fixed

- `tm fleet init` never shortens a hold from `local/state.md`. A hold longer than its 1000-character field, or one that would push the Intent Ledger session record past 2048 B, stops init before the ledger and its hooks are created, and the error names the field, its size and the limit. A `datg.py` session-record size refusal now names the largest fields.
