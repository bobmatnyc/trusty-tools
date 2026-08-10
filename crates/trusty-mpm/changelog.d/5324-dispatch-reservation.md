Fixed

- Two `Agent`/`Task` dispatches issued in one PM turn could both be admitted into
  the same working directory. The `#4480` guard asked the daemon who was already
  writing there and acted on the answer, with nothing claiming the directory in
  between, so both could ask before either was recorded and both see an empty
  set. `POST /api/v1/sessions/{id}/delegations/shared-tree-dispatch` now answers
  and claims in one critical section, so the second of two simultaneous
  dispatches finds the first and is denied. The claim is the delegation record
  the daemon's own `PreToolUse` hook would have written a moment later — same
  correlation key, same liveness, same staleness sweep — so nothing new has to be
  released. Every fail-open branch is unchanged: an unreachable daemon, a
  malformed answer, an unresolvable working directory, an unknown agent, and a
  read-only or isolated dispatch all still allow the call and claim nothing
  (#5324).
