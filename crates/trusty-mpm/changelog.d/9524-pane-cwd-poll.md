Fixed
- A resume that recreates its tmux pane no longer fails with "pane landed at ... tmux silently fell back" on tmux 3.6a, which reports a stale pane directory for about 12 ms after creating a session; the check now re-reads the pane directory for up to 300 ms and still refuses a pane that stays in the wrong directory ([#9524](https://github.com/bobmatnyc/trusty-tools/issues/9524))
