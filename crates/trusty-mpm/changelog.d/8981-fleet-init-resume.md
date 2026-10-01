Added

- `tm fleet init` resumes the Architect's prior Claude conversation when it
  relaunches the Architect, also after the daemon's session record for it was
  deleted or tombstoned. A fresh launch names its conversation id
  (`claude --session-id`) and records it in
  `~/.trusty-mpm/architect-launch/last.architect-conversation`; the next launch
  passes `claude --resume` with that id only when the record names the
  Architect directory, holds a valid id, and Claude Code has the transcript.
  Otherwise it starts a new conversation and the summary says why.
- `tm fleet init` starts the Architect with `claude --remote-control`, so it is
  reachable through Remote Control without `tmux attach` and `/remote-control`.
