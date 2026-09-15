Documentation

- `tm-workflow`'s Git Security Review names `gitleaks` as the primary
  credential scan, keeps the diff as a secondary signal, and warns that
  `detect-secrets` run against paths silently no-ops on a branch that isn't
  checked out and that the sandbox refuses any path containing "secrets"
  (closes [#7972](https://github.com/bobmatnyc/trusty-tools/issues/7972))
- `tm-delegation-patterns` requires `isolation: "worktree"` explicitly on
  every writer dispatch from a main checkout, alongside the existing
  ADR-0048 automatic-grant note
  (closes [#7971](https://github.com/bobmatnyc/trusty-tools/issues/7971))
- `tm-delegation-patterns` says to dispatch a fresh agent at the branch tip,
  not `SendMessage`, when live evidence reopens a task an agent already
  finished
  (closes [#7630](https://github.com/bobmatnyc/trusty-tools/issues/7630))
- `tm-session-pause` states that a dispatched agent's survival across a
  relaunch is conditional, not absolute, and instructs recording each
  agent's dispatch-result id in its `## In Progress` snapshot entry
  (closes [#7970](https://github.com/bobmatnyc/trusty-tools/issues/7970))
