Fixed
- The interactive PM is no longer told to call `glob`, `grep`, `list_dir` or `search_code`, tools its registry never carried — the discovery guidance is now gated on the run's tool registry, so `tcode tui` stops emitting calls the daemon rejects with "no schema registered for tool" (#4602).
