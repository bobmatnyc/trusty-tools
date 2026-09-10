Fixed

- `session_context_pause` now opens its snapshot PR with an explicit `--head <branch>`, so the publish no longer depends on which branch the project checkout happens to be on. The chore branch is built with git plumbing and never checked out, so `gh pr create` read the checkout's own branch and aborted with "you must first push the current branch to a remote, or use the --head flag", stranding the commit locally.
- `tm pr open` takes a `--head <branch>` flag, and derives the component-label diff from that branch rather than the checkout's `HEAD`.
