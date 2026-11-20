Fixed
- `tm architect poll`: a malformed or invalid `local/routines.json` now raises `routine_stale` with fingerprint `config` instead of only logging, so the D5r check cannot switch off silently; a reset non-object `push` state record is logged (#9779).
