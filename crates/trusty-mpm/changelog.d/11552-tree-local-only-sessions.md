Fixed
- `tm tui` tree, Telegram `/fleet` and decommission now count local-only sessions: a session with no `repo_url` binds to the path-form project whose checkout it runs in, including from a `.worktrees/<name>` subdirectory (#11552).
