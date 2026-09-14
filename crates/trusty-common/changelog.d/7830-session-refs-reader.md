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
  - every trust decision is taken from the REF KEY, never from the commit's own
    text: only refs whose `<user-id>` and host-qualified `<session-key>` match
    the caller's `LocalRefIdentity` hydrate, the synthesized `session_id` comes
    from the key rather than the `Session-Id` trailer, and exactly one
    `session-*.md` inside that session's own directory is ever written — a tree
    carrying `sessions-log.jsonl` or a second blob changes nothing on disk
