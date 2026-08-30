Fixed

- `tm hook --pm-guard` now classifies process-substitution bodies. `<(…)` and
  `>(…)` join `$(…)` and backticks as one class of substitution the guard
  decomposes, so `diff <(sed -i s/a/b/ f) x` — which bash executes, and which
  the guard allowed — is refused, and `>(…)` is denied by its body rather than
  incidentally by the `>` redirection scan. An unbalanced opener fails closed,
  matching the `$(`/backtick shape; a quoted `<(…)` in a commit message stays
  literal text. (Refs #2745)
- `tm doctor` gained a `base_clone` check. A linked worktree keeps every source
  file when the clone behind it loses its git internals, and then every git
  command there fails with `fatal: not a git repository` — the 2026-07-21 state
  that went unnoticed for over half an hour across 70 worktrees while both
  worktree probes stayed green. The check reads each live session workspace's
  `.git` pointer and Fails naming the base path, what is missing, and how many
  live worktrees hang off it. Detection only: it never repairs, moves, or
  deletes, and its remediation text keeps the existing quarantine-never-delete
  discipline. (Refs #3605)
- Deploy diagnostics now reach the operator on every in-process CLI path. Bare
  `tm` (including the in-place relaunch), `tm doctor --fix-skills`, and
  `tm catalog apply` each run a deploy that legitimately declines files — a
  checksum-frozen skill, an unreadable ledger, a raced merge — and each decline
  was written to a `tracing` subscriber that was never registered, so the file
  stayed stale forever with no signal. All three now install the same
  stderr-only subscriber `tm sessions instructions` uses, at the `warn` default
  so a clean run stays quiet. Registration is `try_init`, so a second
  installation returns an error instead of aborting the process. (Refs #4878)
- Peer-bus registration no longer overwrites a live instance on an instance-id
  suffix collision. `DashMap::insert` replaced the existing entry, after which
  every lookup of the first instance resolved to the second — instance-addressed
  delivery reaching an instance the sender did not name. Registration now claims
  the key through `entry`, re-minting up to eight times and logging each
  collision, and returns the new `BusError::InstanceIdCollision` (HTTP 409)
  rather than displacing a registered instance. (Refs #4276)
