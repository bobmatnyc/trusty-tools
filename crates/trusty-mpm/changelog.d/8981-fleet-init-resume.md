Added

- `tm fleet init` resumes the Architect's prior Claude conversation when it
  relaunches the Architect, also after the daemon's session record for it was
  deleted or tombstoned. A fresh launch names its conversation id
  (`claude --session-id`) and records it in
  `~/.trusty-mpm/architect-launch/last.architect-conversation`; the next launch
  passes `claude --resume` with that id only when the record names the
  Architect directory, holds a valid id, is a regular file (not a symlink or
  FIFO) owned by the current user and not writable by other users, and Claude
  Code has the transcript. Otherwise it starts a new conversation and the
  summary says why.
- A resume whose `claude` is proven gone within 5 seconds fails
  `tm fleet init`: it kills the session it started and clears the
  conversation record, naming its path, so the next `tm fleet init` starts a
  new conversation instead of retrying the dead one. When tmux will not kill
  the session, the error says so and names the `tmux kill-session` command to
  run. When tmux or the process table cannot be read twice, `tm fleet init`
  keeps the session and the record and prints a warning naming both.
- `tm fleet init` starts the Architect with `claude --remote-control`, so it is
  reachable through Remote Control without `tmux attach` and `/remote-control`.
