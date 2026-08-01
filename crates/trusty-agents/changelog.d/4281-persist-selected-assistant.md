Added

- The selected assistant now survives an app relaunch (closes [#4281](https://github.com/bobmatnyc/trusty-tools/issues/4281)).
  `activeAgentId` was the last significant piece of desktop-client state that
  did not persist — a plain in-memory `writable<string | null>(null)` that reset
  to Concierge on every reload, while the API token and the theme already
  round-tripped through `localStorage`. Since "Assistant" is a TYPE and `izzie`
  / `cto-assistant` are INSTANCES of it, which instance is selected is
  user-meaningful state. The store now seeds from `localStorage` at import time
  and writes back through a subscription, so every selection site — the chat
  header switcher, the sidebar's workstream resume, and the assistant picker
  coming in [#4404](https://github.com/bobmatnyc/trusty-tools/issues/4404) —
  sticks by simply setting the store; there is no separate save call to forget.
  Per the owner decision of 2026-07-28 the stickiness is per-app-launch
  (last-used), NOT per-workstream: filtering to a workstream does not re-select
  an assistant, which would cut against DOC-54 §9.2's "workstreams are filters,
  not containers".
  - Degrades on every axis a hand-editable file can fail: a missing key, a
    corrupt value (evicted so it is never re-parsed), an explicit Concierge
    selection, and a `localStorage` that is absent or throws all resolve to the
    pre-existing Concierge default without throwing or blocking startup.
  - A stale selection naming an assistant that no longer exists is demoted to
    Concierge as soon as a POPULATED roster contradicts it. An empty roster is
    deliberately not treated as stale — the cold-start catalog fetch races the
    sidecar and fails outright when the API is unreachable, so "no entries"
    means "not loaded", never "your assistant is gone".
