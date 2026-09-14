Added

- `catchup::session_refs` — the read side of ADR-0062 session-history refs
  (refs [#7830](https://github.com/bobmatnyc/trusty-tools/issues/7830))
  - `ensure_fetch_refspec` registers `+refs/tm/sessions/*:refs/tm/sessions/*` on
    `remote.origin.fetch` idempotently; without it a plain clone sees no session
    refs at all
  - `list_session_refs` is the `git for-each-ref` aggregator over
    `refs/tm/sessions/**`
  - `hydrate_session_cache` rebuilds `.trusty-mpm/sessions/` from each ref's tip
    tree, synthesizing the `sessions-log.jsonl` pause line that attributes each
    restored snapshot, and never overwriting a file already on disk
