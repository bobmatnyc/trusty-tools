Fixed
- ADR-0057's `sole-owner` re-check no longer treats an unanswered daemon as an
  empty owner set. It read the shared-tree route through
  `live_shared_tree_writers`, whose fail-open contract maps an unreachable
  daemon, a timeout, a 5xx and an unparseable body all to an empty vec — which
  is indistinguishable from "nobody holds this tree". A `version-control` agent
  could therefore remove a worktree another agent was writing in whenever the
  daemon was down, with the ownership check never having run. The re-check now
  goes through a fail-closed `live_shared_tree_writers_or_deny` and denies on
  every non-answer, naming `sole-owner` and the daemon-side reason. A payload
  with no `session_id` denies for the same reason: no session's delegations can
  be addressed without one. The HEAD-move rule's fail-open reader is untouched —
  the two biases are kept as two functions, because a wrong ALLOW there costs
  the operator a `git merge` on their own checkout and here costs another
  session its work.
