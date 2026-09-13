Changed
- `BASE-ENGINEER.md` now tells the engineer to prove a regression test RED
  against the branch's own pre-fix commit (`git merge-base origin/main HEAD` at
  task start, or the SHA the brief names) instead of a bare `origin/main`, which
  moves while the work is in progress. The same section adds the gate-script
  spelling rule: run a repository script as `./scripts/<name>.sh`, never
  `bash scripts/<name>.sh`, which an isolation worktree can refuse (#7705).
