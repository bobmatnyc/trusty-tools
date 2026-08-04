Security

- `decommission_record_only` no longer tears down the session runtime, so `tm ls`'s auto-prune can never SIGTERM or kill a live tmux session (closes [#4728](https://github.com/bobmatnyc/trusty-tools/issues/4728))
  - the `graceful_terminate_runtime` call sat above the `record_only` guard and ran unconditionally; its only self-guard is live-tmux name membership, never the record's captured `pane_id`
  - reachable without any name collision: `is_unresumable` never consults tmux liveness, and `mark_errored` never touches the pane, so a stopped/errored record with a deleted workspace and a live pane was a kill target
  - present in 1.3.3 and 1.3.4
