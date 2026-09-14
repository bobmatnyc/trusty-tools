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
  - `refs/tm/sessions/**` has no server-side access control, so hydration reads
    exactly ONE ref — the caller's own `SessionRefTarget` — rather than every
    ref under a matching user id. A self-consistent sibling ref pushed under the
    victim's login would otherwise be materialized, and a future-dated snapshot
    with a matching `## Tmux Window` body wins the resume's newest-first pick
  - within that ref, only the single `session-*.md` the commit names is written,
    at a path that session may own; the tree is never walked and the
    `Session-Id` trailer is never read, so a ref carrying `sessions-log.jsonl`
    or a second blob changes nothing on disk
  - a checkout with no `origin` is refused before any config write — adding the
    refspec there used to create a phantom `origin` with an empty URL
