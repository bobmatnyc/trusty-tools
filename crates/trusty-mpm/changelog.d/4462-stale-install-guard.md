Added

- **`tm reinstall --binary` refuses a source behind `origin/main`.** `tm` is one
  global binary shared by every managed session on the machine, and the `--path`
  reinstall route rebuilds it from whatever source directory cargo's ledger
  recorded — so installing from a worktree that predates a fix regressed that
  fix for every session at once, silently, because the binary's own version
  number does not move when its source is merely stale. The command now fetches
  and compares `HEAD` against `origin/main`, refusing when it is behind and
  naming `TRUSTY_MPM_ALLOW_STALE_INSTALL=1` as the deliberate override. It
  refuses on positive evidence only: a source that is not a git repository, has
  no `origin/main`, or has no usable `git` warns and installs
  ([#4462](https://github.com/bobmatnyc/trusty-tools/issues/4462))
