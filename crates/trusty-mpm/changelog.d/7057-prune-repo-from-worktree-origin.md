Fixed

- `tm session prune-worktrees --merged-prs` now pins every pull-request lookup to the repository the target worktree's own `origin` remote names, instead of letting `gh` infer one from the working directory — a run for one registered project could resolve against another project on the same machine and reclaim nothing (#7057).
- The `git worktree remove` guard's merged-pull-request refusal names the repository it searched, so a lookup aimed at the wrong repository is visible instead of reading as "no pull request found" (#7057).
- A worktree whose `origin` is missing or names no GitHub `owner/repo` now blocks removal with that reason instead of asking `gh` to guess a repository (#7057).
- A worktree on a non-`github.com` remote — GitHub Enterprise Server, or a host a `GH_HOST` override points at — now resolves to a host-qualified `host/owner/repo` slug, so `gh --repo` asks that server instead of silently answering from github.com's same-named repository (#7057).
