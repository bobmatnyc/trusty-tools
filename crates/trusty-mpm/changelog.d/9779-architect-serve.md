Added
- `tm architect serve [--shadow] [--once]`: runs the Architect poll cycle on local events (inbox, polled tmux pane activity, transcripts), at most one event cycle per 15 s, with a 60 s tick fallback; it attaches no tmux client, so `tm ls -a` and session switching are unaffected; `--shadow` writes only `inbox/shadow/` (#9779).
