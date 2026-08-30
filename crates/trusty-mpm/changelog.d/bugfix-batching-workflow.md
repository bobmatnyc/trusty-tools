Documentation

- **Bugfix batching is now the default for same-module bug clusters.** The `tm-workflow` skill's "One Outcome, One PR" section states that several open bugs constrained to the same file, module, or tightly-coupled area go in one PR — one engineer, one worktree, one review, one CI pass — instead of one PR per issue, with each issue keeping its own regression test and `Refs #N`. `tm-delegation-patterns` adds the matching anti-pattern row: one PR per issue for same-module bugs is the anti-pattern; the fix is one batched dispatch/PR per file-cluster.
