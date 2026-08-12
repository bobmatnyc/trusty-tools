Fixed

- A standing rule written over HTTP or from chat now reaches the prompt context instead of being stored and ignored (closes [#5524](https://github.com/bobmatnyc/trusty-tools/issues/5524), closes [#4905](https://github.com/bobmatnyc/trusty-tools/issues/4905))
  - `POST /api/v1/palaces/{id}/kg` and the chat assistant's `kg_assert` tool both
    stored a hot-predicate triple and never rebuilt the prompt cache, so the fact
    stayed invisible to every later turn while both surfaces reported success.
    Which client a user happened to reach for silently decided whether their fact
    took effect
  - the cause was duplication, not two independent oversights: six surfaces each
    carried their own copy of the admission → assert → refresh sequence, and the
    refresh step drifted out of two of them. All six now route through one entry
    point, `kg_write::assert_triple`, so a new write surface gets the whole
    sequence by construction and a behaviour fix lands once
  - a failed prompt-cache rebuild is now reported as `KgWriteError::CacheRefresh`
    instead of being swallowed as a `warn!` on four separate paths. "Stored but
    invisible" is the exact defect this change removes, so it is no longer a
    condition a caller can mistake for success. Behaviour change: an HTTP assert
    whose rebuild fails answers 500 rather than 204 — the triple is in storage
    either way, and the arm is unreachable today (see below)
  - the Tier S admission gate (#4888) is unchanged in effect on every path, and
    the refusal text still names the occupying facts and `remove_prompt_fact`
  - known limitation, unchanged by this fix and now documented at the call site:
    `gather_hot_facts` logs and skips a palace it cannot read, then returns `Ok`.
    A transient read failure therefore truncates the rebuilt cache — and the
    Tier S occupancy count that gates admission — without reporting anything
