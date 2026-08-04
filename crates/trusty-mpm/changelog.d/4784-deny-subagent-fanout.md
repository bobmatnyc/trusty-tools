Added

- `tm hook --pm-guard` now denies `Task`/`Agent` dispatch when the calling session is itself a subagent (closes [#4784](https://github.com/bobmatnyc/trusty-tools/issues/4784))
  - the PM keeps dispatching; only fan-out from within a subagent is blocked
  - `SendMessage` is never denied, so a blocked agent can always report back
  - fails OPEN — an indeterminate caller context allows the dispatch rather than risking a false deny against the PM
