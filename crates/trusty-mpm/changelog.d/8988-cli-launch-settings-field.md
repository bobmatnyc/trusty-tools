Breaking
- `CliLaunch` gains a public `settings` field carrying the per-session `--settings` path for the owner launch policy (#8988). Code that builds a `CliLaunch` with a struct literal must set it (`None` keeps the old launch).
