Changed

- The `tm projects` TUI's `[d]` decommission hotkey now routes through the same
  shared implementation as every other entry point, so it too repairs the base
  repo's worktree bookkeeping. It called the daemon endpoint directly and left a
  stale `git worktree list` entry behind — the same bug the rest of #5913 closes.
- Every decommission entry point now routes through one implementation,
  `CommandExecutor::decommission_managed_id`
  (closes [#5913](https://github.com/bobmatnyc/trusty-tools/issues/5913)). The
  bulk prune sweep used to reach the endpoint through its own hand-rolled
  `reqwest` POST, independent of the routed `tm session decommission <id>` verb;
  that split is what let #5899's wording divergence exist, and it carried a
  second divergence with it — only the bulk path repaired the base repo's
  worktree bookkeeping. `tm session decommission <id>` now runs
  `git worktree prune` on the same signal the sweep always used, so
  decommissioning a tm-owned worktree interactively no longer leaves a stale
  entry in `git worktree list`.
- A 404 from the decommission endpoint is mapped to an error naming the session
  once, in `DaemonClient::decommission_managed_session`, rather than at each
  entry point. `tm session prune-idle`'s fail-closed sweep still records a
  missing session as a failed row, and no longer depends on which caller issued
  the request.
