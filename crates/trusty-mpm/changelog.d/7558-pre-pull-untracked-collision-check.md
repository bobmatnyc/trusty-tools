Fixed

- The bundled `git-workflow` skill now states a pre-pull untracked-collision
  check, so a `git pull --ff-only` that would abort with "The following
  untracked working tree files would be overwritten by merge" is caught before
  it runs rather than improvised around afterwards. The guidance names the
  exact check — `git status --porcelain` for the `??` rows, then
  `git show origin/<base>:<path> | diff - <path>` per colliding path — and its
  two outcomes: identical content resolves itself by deleting the untracked
  copy, differing content is reported with the diff. Deleting an untracked path
  to clear the abort without diffing it first is called out as the thing not to
  do, since the abort only says two writers produced one path, never that
  either copy is disposable (#7558).
- `tm-workflow`'s main-checkout refresh instructions now point at that check.
  The refresh is where `pull --ff-only` is actually prescribed, and it already
  named one cause of the command failing — another session's uncommitted work —
  so the untracked-path collision is recorded there as the other cause, with a
  cross-reference rather than a second copy of the procedure (#7558).
