Fixed

- A session record whose tmux pane id was captured before a tmux server
  restart no longer owns a live session that reuses its name and pane id.
  Records now store the tmux server instance (`<pid>:<start_time>`) with the
  pane id, and stop, delete and decommission leave a session on another server
  running (#9004).
- Limit: a session that was live when this version was installed has no
  server identity, so tm cannot kill its tmux session. Stop, delete and
  decommission leave it running. Resuming it while its tmux session is still
  live re-attaches to the pane and does not record the server. The record gets
  a server identity only after its tmux session ends and a resume creates a
  new one (#9004).
- The stop and decommission teardown now kills the record's tmux session by
  its `$N` session id, read in the post-grace ownership re-check, instead of
  by its reusable name (#9004).
- A pane id capture that cannot read the pane's server, or that reads a pane
  now in another session, logs a warning and stores no server identity
  (#9004).
