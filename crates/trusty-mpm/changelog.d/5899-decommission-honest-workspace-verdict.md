Fixed

- `tm session decommission <id>` no longer reports "workspace removed" when the
  workspace is still on disk
  (closes [#5899](https://github.com/bobmatnyc/trusty-tools/issues/5899)).
  The daemon's `workspace_removed` verdict was dropped client-side — the response
  decoded into `ManagedSessionSummary`, which has no field for it, so serde
  discarded it silently — and the CLI printed a hardcoded success line for every
  decommission, including adopted and local-path workspaces it never touched. The
  verdict now rides a purpose-built `ManagedDecommissionOutcome` through the
  executor to one shared message helper: it reports removal only when the daemon
  says the removal happened, says the workspace is still on disk when it says
  otherwise, and says the outcome is unreported when an older daemon sends no
  verdict. `tm ls` tombstone rows also need
  [#5897](https://github.com/bobmatnyc/trusty-tools/issues/5897) to clear.
