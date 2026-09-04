Added
- `tm pr merge <n> [--auto] [--no-delete-branch]` squash-merges a PR with its
  own validated body as the landing commit message. It reads the PR once
  (`gh pr view <n> --json number,title,body,isDraft,labels,reviewDecision,mergeStateStatus,headRefName`),
  re-runs the seven-field-and-footer body check `tm pr open` runs, then calls
  `gh pr merge <n> --squash --delete-branch --subject "<title> (#<n>)"
  --body-file <tmp>`. `gh pr merge --squash` on its own lets GitHub assemble the
  squash commit from the branch's raw commit messages, so the validated body
  never reached `main` — the squash for PR #6607 landed as a concatenation of
  five raw messages, harness trailers included
  ([#6808](https://github.com/bobmatnyc/trusty-tools/issues/6808)).
- The command refuses with one line, a non-zero exit, and no merge call when the
  body fails validation, the PR is a draft, it carries a `do-not-merge` label in
  any case, its review decision is `CHANGES_REQUESTED`, or its
  `mergeStateStatus` is `CONFLICTING` — that last naming `gh pr update-branch`.
  A `mergeStateStatus` of `BEHIND` is not a refusal: a behind branch merges fine
  on this repo. With `--auto` the merge is queued and GitHub applies the
  supplied subject and body when auto-merge fires.
