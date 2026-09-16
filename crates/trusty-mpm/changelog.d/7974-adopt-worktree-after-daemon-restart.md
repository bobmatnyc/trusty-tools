Fixed

- `tm session adopt-worktree` now salvages a dead dispatched agent's tree after a daemon restart: when the ADR-0045 delegation registry holds no record, the git worktree lock is consulted and a lock naming a pid the kernel does not have is accepted as evidence the agent ended, logged with that reason. A live owner, a lock naming a running pid, and a lock that cannot answer are all still refused (refs [#7974](https://github.com/bobmatnyc/trusty-tools/issues/7974))
