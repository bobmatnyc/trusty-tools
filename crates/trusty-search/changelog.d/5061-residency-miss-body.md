Fixed

- `GET /indexes/:id/status` and `GET /indexes/:id/chunks` now return a JSON body with their `503`/`404` instead of a bare status code. An MCP caller previously saw `returned 503 Service Unavailable: ` with empty text and could neither tell a cold-parked index from a permanently restore-failed one nor learn how to clear it (#5061).
- The cold-parked `503` from `status`, `chunks`, and `grep` now carries `restore_via: "POST /indexes/{id}/search"`, naming the one endpoint that reloads the index. These three cannot reload it themselves, so without the hint a caller polled a `503` that nothing else would clear while a plain `search` on the same id self-healed (#5061).
- `status`, `chunks`, and `grep` now render the not-resident / restore-failed / unknown verdict through one shared builder, so the three can no longer report the same daemon state three different ways (#5061).
