Fixed
- The tmux-gone reaper judges each managed session on its own tmux server, so a PM live on its workspace server is never stopped because the default server does not list it (#10101).
- A workspace tmux server that cannot be listed, or does not answer within 2 s, leaves only its own sessions unknown (treated as live); the default server and every other workspace server still reconcile (#10101).
- A session whose stored tmux socket is outside the current tmux socket directory (`TMUX_TMPDIR` changed) is refused by name routing and judged live by its own socket (#10101).
