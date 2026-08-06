Changed

- `ticketing` agent must label every issue at creation with three families — one type (`bug`/`enhancement`/`refactor`/`chore`/`documentation`/`epic`), one or more component/crate labels, and `P0`–`P3` only when the issue text itself asserts severity — on top of the existing `--assignee @me` / `trusty-mpm` / `ws/<session>` defaults
  - milestones are release slots, not a field every issue receives: left unset unless the issue is deliberately scheduled into a release confirmed open, and never used to hold a `ws/` workstream value
  - the agent's report now names the labels applied and the milestone state for anything it created
  - `tm-ticketing` states that the two harness defaults are not the whole label set and points at the agent asset for the rest
