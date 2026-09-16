Documentation

- `version-control` now states that a 5xx or timeout from a mutating `gh`
  call (`gh pr merge`, `gh pr create`, `gh api -X POST/DELETE`) is not proof
  the call failed — it reads the state back (`gh pr view --json
  state,mergeCommit`) before retrying
  (refs [#8013](https://github.com/bobmatnyc/trusty-tools/issues/8013))
