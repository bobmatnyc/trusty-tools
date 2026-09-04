Added
- `tm pr merge <n> [--auto] [--no-delete-branch]` squash-merges a PR with its
  own validated body as the landing commit message. It reads the PR once
  (`gh pr view <n> --json number,title,body,isDraft,labels,reviewDecision,mergeStateStatus,mergeable,headRefName`),
  re-runs the seven-field-and-footer body check `tm pr open` runs, then calls
  `gh pr merge <n> --squash --delete-branch --subject "<title> (#<n>)"
  --body-file <tmp>`. `gh pr merge --squash` on its own lets GitHub assemble the
  squash commit from the branch's raw commit messages, so the validated body
  never reached `main` — the squash for PR #6607 landed as a concatenation of
  five raw messages, harness trailers included
  ([#6808](https://github.com/bobmatnyc/trusty-tools/issues/6808)).
- The command refuses with one line, a non-zero exit, and no merge call when the
  body fails validation, the PR is a draft, it carries a `do-not-merge` label in
  any case, its review decision is `CHANGES_REQUESTED`, or it has merge
  conflicts — `mergeable` reporting `CONFLICTING` or `mergeStateStatus`
  reporting `DIRTY`, the two disjoint enums GitHub splits that answer across.
  The conflict refusal names `gh pr update-branch`.
- What it deliberately does not refuse: `BEHIND` (a behind branch merges fine on
  this repo), and every other `mergeStateStatus` — `BLOCKED`, `UNSTABLE`,
  `HAS_HOOKS`, `UNKNOWN` — plus every `reviewDecision` other than
  `CHANGES_REQUESTED`, all of which are left to `gh pr merge` to accept or
  reject. With `--auto` the merge is queued and GitHub applies the supplied
  subject and body when auto-merge fires; under a merge queue GitHub ignores
  them, and this repo has no merge queue.
