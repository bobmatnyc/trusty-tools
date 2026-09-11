Fixed

- `tm hook --pm-guard` now puts a Bash file write through the ADR-0044 /
  ADR-0048 main-checkout write boundary, the rule that until now only an
  `Edit`/`Write` call took. A shell write reached only `SHELL_EDIT_REASON`,
  which asks WHO is writing: it allows the first writes of a turn and both
  subagent markers skip it, so `git diff --output=<file>` and
  `echo … > <file>` each landed a source file in a shared main checkout with
  nothing refused (#7399).
- The boundary reads its target from the one detector `pm_guard_bash` already
  keeps — a redirect target, or the file a git write option names — so
  `git diff --output=`, `git log --output=` and a `>` redirect all reach one
  deny with one message. Denied when the file is source and lands in a main
  checkout; allowed inside a worktree, for a document or configuration file,
  and anywhere outside a checkout (#7399).
- Reads are untouched: `git diff --no-index a b`, a plain `git diff`,
  `git format-patch --stdout`, and a `sed -n` whose trailing token names a file
  it only reads are all still allowed — the boundary acts only on a write it
  can positively identify (#7399).
