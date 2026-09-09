Changed

- The panic-safe `TerminalGuard` and the raw-mode/alternate-screen setup that the coordinator chat and the `project_ctl` dashboard each carried privately now live once in `trusty_mpm::tui::terminal`, shared with the new session TUI. `suspend`/`resume` are new there: they hand the real screen back for a tmux attach and take it again on detach without dropping the terminal (#7224).
- `session_manager::rename::validate_session_name` is now `pub`. The session TUI validates a typed name with the daemon's own function before issuing the PATCH, so its inline rejection message is the one the round trip would have returned rather than a second spelling of the rule (#7224).
