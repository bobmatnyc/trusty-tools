Fixed

- Stopping, deleting, pruning or decommissioning a stale session record no
  longer kills a live tmux session that has since taken its name. The
  teardown now signals and kills only a session that still holds the record's
  own tmux pane (`%N` id), and signals that pane's `claude`, not the
  session's active pane. When the pane is gone, the record has no pane id, or
  tmux cannot list the panes, the record moves and nothing is signalled or
  killed. `tm session stop`, `tm session delete` and the `tm ls` delete say
  "record only" and name the session they left running.
- `tm session delete` of a stale record whose name a different live session
  now uses no longer needs `--force`; a delete still never touches tmux.
