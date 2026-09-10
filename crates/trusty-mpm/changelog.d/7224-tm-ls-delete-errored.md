Fixed
- `tm ls` now deletes an errored session by stopping its runtime first, as one confirmed action, instead of refusing with a hint to run `tm session stop` in a shell. A stop the daemon rejects issues no delete at all, and the picker's status line wraps instead of clipping, so a refusal's session id is never cut mid-token.
