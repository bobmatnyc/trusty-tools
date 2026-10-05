Added
- `tm memory remember` and `tm memory note` accept `--fact-key <key>` and `--expires-at <RFC 3339>`, passed to trusty-memory as `fact_key` / `expires_at`. A write under a key supersedes the prior fact in that slot (`ws:<session>/resume`, `pr:<n>/state`). An unparseable `--expires-at` is refused before anything is sent. Omitting both flags sends the same request as before (#9142).
