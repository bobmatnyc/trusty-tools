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
- The ADR-0048 worktree grant applies the same split. That call computes the
  #4480 concurrency verdict before the grant rather than skipping it — "a
  dispatch made into a checkout another writer holds is denied, and only an
  empty answer is granted" — so a running daemon that leaves it unanswered now
  denies there too. The grant it would otherwise emit is a rewrite of the
  dispatch's arguments that `tm` cannot confirm the harness applies, and in a
  main checkout ADR-0048 states the cost: a false allow corrupts another
  session's branch. An absent daemon still grants, and now warns on stderr that
  the check did not run.
- The read-only writer query behind the `git merge` / `git rebase` /
  docs-commit rules keeps its fail-open contract. A deny there blocks ordinary
  git work on the operator's own checkout rather than admitting a second agent.
