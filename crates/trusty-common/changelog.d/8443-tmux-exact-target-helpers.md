Added
- `tmux::exact_session_target`, `exact_window_target`, `exact_pane_target`, `shell_exact_session_target`, `shell_attach_command`, `is_immutable_id` and `check_session_name` are the one place a tmux target is spelled. They normalize `:` and `.` in a session name to `_`, matching the name tmux actually stores (#8443).
- `TmuxTarget::try_session`, `TmuxTarget::validate`, `TmuxCommand::validate_targets` and `TmuxTargetError` let a caller refuse an empty session name before tmux runs (#8443).
