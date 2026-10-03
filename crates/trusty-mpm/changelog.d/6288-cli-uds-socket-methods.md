Added

- The daemon socket serves `mpm.build_lease.decision`,
  `mpm.builder_slot.claim`, `mpm.builder_slot.list` and
  `mpm.managed.adopt_worktree`, each through the same body as its HTTP route
  (#6288).
- `DaemonClient::over_socket` and `DaemonClient::from_resolved_socket` build a
  client that sends every request to the daemon's unix socket and refuses a
  route the socket does not serve rather than falling back to TCP (#6288).
