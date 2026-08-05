Added

- Wing MCP surface — `wing_list`, `wing_create`, `wing_rename` (ADR-0027 D2, ticket T9, closes [#4809](https://github.com/bobmatnyc/trusty-tools/issues/4809))
  - `wing_list(palace)` is the discovery primitive: every wing with its label,
    description, room count, and whether it is the default
  - `wing_create(palace, label)` is idempotent — case-insensitive matching, first-seen
    spelling kept for display, and creating `default` returns the palace's existing
    default wing rather than a second one
  - `wing_rename(palace, wing, new_label)` retires the old label (a rename, not an
    alias) and provably touches no room and no drawer, since rooms reference a wing
    by id
  - `memory_remember` gains an optional `wing`, so a write can place its room in a
    named scope; `memory_recall` and `memory_list` gain an optional `wing` that
    restricts results to the rooms that wing owns
  - omitting `wing` everywhere is the pre-wing behaviour exactly — the argument is
    never required, and wing-less writes land in the palace's default wing
  - an unknown `wing` is a loud error naming `wing_list`, never a silent empty
    result; `memory_list` likewise rejects `wing` and `room` together rather than
    honouring one and dropping the other
  - `memory_recall_deep` takes `wing` too, so no recall path silently ignores a scope
  - the `room`/`wing` scope is resolved BEFORE the embedder-warming short-circuit, so a
    scoped recall issued while the daemon is warming is filtered rather than returned
    unscoped
