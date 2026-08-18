Fixed

- `tm hook --pm-guard` no longer admits a concurrent shared-worktree dispatch
  when the daemon fails to answer
  (closes [#5923](https://github.com/bobmatnyc/trusty-tools/issues/5923)).
  `post_shared_tree` returned the same "nobody else is here" for all five of its
  failures, so a request timeout, a 5xx and an unparseable body allowed the
  dispatch the guard exists to deny — and the 2 s budget is reachable on an idle
  machine, which put the guard off under exactly the load that makes two
  concurrent dispatches likely. The failures now split by what they say about
  the daemon: a daemon that is listening and does not answer usably DENIES, with
  a message naming the failure and the remedies that need no daemon; a daemon
  that is not listening, or that 404s the route because it predates it, still
  allows and now says on stderr that the guard is not enforcing.
- Only the dispatch-claim path changed. The read-only writer query behind the
  `git merge` / `git rebase` / docs-commit rules and the ADR-0048 granted-worktree
  path keep their fail-open contract, since a deny there blocks ordinary git work
  or refuses an isolated writer the shared HEAD was never at risk from.
