Fixed
- `tm doctor` probes a record on a workspace tmux server on that server: the `session_names` row reads its pane and `claude` pid there instead of on the default server. Every doctor probe of a workspace server (`session_names`, `tmux_options`) is bounded and at most one runs per server at a time, so a hung server reads as `Unknown` naming that server instead of hanging the run (#10101).
