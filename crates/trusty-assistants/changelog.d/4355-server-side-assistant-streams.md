Added

- Tasks now record which assistant they were addressed to, and the API can
  return one assistant's task stream in a single call (refs
  [#4355](https://github.com/bobmatnyc/trusty-tools/issues/4355),
  [#4278](https://github.com/bobmatnyc/trusty-tools/issues/4278)). Selecting a
  roster entry on `POST /api/task` used to be consumed at dispatch to pick a
  code path and then discarded, so nothing downstream could say which
  conversation a task belonged to. `PmResponse` now carries `addressed_agent` —
  who was ASKED, distinct from the existing `responder_agent`, which records who
  ANSWERED and is populated only when a turn delegated. A turn addressed to
  `assistant` that delegates to `izzie` stays in the `assistant` stream while
  the bubble is still labelled `izzie`. A submission with no roster selection
  is attributed to the Concierge (`ctrl`), matching what a null selection
  already means in the desktop client.
  - `GET /api/tasks` accepts `?agent=<name>` and `?limit=<n>`. Omitting both
    returns the previous unfiltered listing unchanged; a blank `agent` is
    treated as absent, and an assistant with no history returns `[]` rather
    than an error.
  - Retention is now per assistant rather than server-wide. The old single cap
    of 20 would have divided across the roster once streams were split — about
    three turns of history each for six assistants — so each stream keeps its
    own 20, with a 200-row server-wide backstop that bounds memory when a
    client mints unbounded distinct agent names. A single-assistant user sees
    exactly the depth they saw before. Eviction still never takes a running
    task, and cancelling a task no longer moves it out of its own stream.
  - A `tasks.json` written by an earlier version still loads: the new field
    defaults rather than failing, so upgrading does not discard task history.
    Its rows land in the Concierge stream.

Note: this is the server half only. Wiring the desktop client to load a stream
on assistant switch is a separate change.
