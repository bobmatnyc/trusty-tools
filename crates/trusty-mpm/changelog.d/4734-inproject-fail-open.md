Fixed

- A git failure while reading `remote.origin.url` no longer downgrades an
  in-project session's isolation (refs
  [#4734](https://github.com/bobmatnyc/trusty-tools/issues/4734)).
  `inproject::get_origin_url` returned a bare `Option`, so "this repo has no
  `origin` remote" and "the git invocation itself failed" were the same value,
  and `try_inproject_spawn` read that value as the former — falling through to
  a spawn in the operator's live checkout with no protected base clone, no
  worktree isolation, and no push-guard hook, on a directory it had just
  confirmed has a `.git` entry. It now returns `Result<Option<String>, String>`:
  a git-exec failure or a fatal exit (128 for an unreadable `.git/config`, a
  dangling gitdir pointer, or a `safe.directory` ownership refusal) is `Err`,
  while git-config's exit 1 — its "the key is not set" answer, and what both a
  plain non-git directory and a repo without an `origin` produce — stays
  `Ok(None)` and still falls through by design.
- `tm launch` and the guided default no longer report an unreadable remote as a
  missing or non-GitHub one, so the remediation they print (`tm connect`, or
  "this is not a GitHub repository") is no longer wrong advice for a repo whose
  config git declined to parse.

Changed

- `ColdStartError` gained `OriginUnreadable`, so a managed checkout whose remote
  git cannot read is no longer reported as `NoOrigin` — which told the operator
  to move or remove a directory that may be fine.

Breaking

- `daemon::managed_routes::inproject::get_origin_url` now returns
  `Result<Option<String>, String>` rather than `Option<String>`, and
  `ColdStartError` has a new variant. Callers that want the previous fail-open
  behaviour spell it explicitly with `.ok().flatten()`.
