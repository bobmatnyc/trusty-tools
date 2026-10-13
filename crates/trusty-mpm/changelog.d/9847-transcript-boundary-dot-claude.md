Fixed
- With `CLAUDE_CONFIG_DIR` unset, a hook or statusline `transcript_path` must sit under `~/.claude`; any other file under `$HOME` is refused. Before, the boundary was `$HOME` itself. A host with no `~/.claude` refuses every transcript path (Refs #9847).
