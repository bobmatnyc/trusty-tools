Fixed

- Session tmux calls reach the workspace server a PM was launched on: `tm sessions output` for a managed session no longer captures pane `%N` on the default server, `tm sessions attach` prints `tmux -S '<socket>' attach-session …`, and answer delivery, the stop snapshot, prune's liveness gate, the worktree inventory's cwd read and the picker's has-session probe and attach hint all ask the session's own server ([#10101](https://github.com/bobmatnyc/trusty-tools/issues/10101))
