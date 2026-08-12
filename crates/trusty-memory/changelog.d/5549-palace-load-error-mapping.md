Fixed

- `PATCH /api/v1/palaces/{id}` no longer answers 404 when it cannot determine whether the palace exists. Both rename paths mapped every `PalaceStoreError` through `not_found`, so a denied or transient stat of `palace.json` told the client the palace does not exist — erasing, at the caller, the distinction `load_palace` draws. A genuine absence is still 404; anything else is now 500 and says it could not load the palace (#5549, ADR-0045).
- The startup migration `migrate_default_palace_name` no longer skips silently when it cannot stat `localLLM/palace.json`. Its `exists()` pre-check read a denial as "no default palace on this host" and no-op'd every boot without reaching the propagation below it; the pre-check is gone and only a genuine `NotFound` is a no-op (#5549).
