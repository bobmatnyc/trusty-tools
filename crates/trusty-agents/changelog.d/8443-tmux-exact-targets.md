Fixed
- The tmux orchestrator and the debugger's tmux adapter now address sessions by exact target (`=<name>` for session verbs, `=<name>:` for window and pane verbs), so an operation on a missing session can no longer land on a live session whose name starts with the same text (#8443).
- The attach hint `tagent` prints, and the one the web UI shows, is now `tmux attach-session -t '=<name>'` (#8443).
