Fixed

- A session record whose tmux pane id was captured before a tmux server
  restart no longer owns a live session that reuses its name and pane id.
  Records now store the tmux server instance (`<pid>:<start_time>`) with the
  pane id, and stop, delete and decommission leave a session on another server
  running. A record written before this change has no server identity, so its
  teardown is record-only until it is resumed or recreated (#9004).
- The stop and decommission teardown now kills the record's tmux session by
  its `$N` session id, read in the post-grace ownership re-check, instead of
  by its reusable name (#9004).
