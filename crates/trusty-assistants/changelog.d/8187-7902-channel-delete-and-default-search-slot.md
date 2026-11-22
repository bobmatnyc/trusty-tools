Added

- `DELETE /api/channels/{id}` removes one global channel under the same
  channel-write credential and revision compare-and-swap as the create and the
  update ([#8187](https://github.com/bobmatnyc/trusty-tools/issues/8187)). It
  refuses with `409` naming the assistants whose bindings still overlay the
  channel unless the request carries `force=true`, in which case those bindings
  are left on disk and reported as `inert_bindings`.
- The delete response carries `receiving_until_restart`
  ([#8187](https://github.com/bobmatnyc/trusty-tools/issues/8187)). Listener
  poll loops are spawned once at API startup from a copy of the channel config,
  so deleting a `receive_enabled` channel does not stop its receiver — it keeps
  polling and keeps waking its captured `route_to` until the daemon restarts,
  and the response now says so instead of leaving it to be discovered from the
  wake log. Stopping a running poller on delete is follow-up work, deliberately
  not in this change.
- A forced delete emits its own audit line (`audit="channel-write-forced"`)
  carrying `forced=true` and the assistants left inert, beside the shared
  count-only channel-write record.
- A delete that cannot determine what references the channel names the
  assistant that blocked it, in the log and in the `500` body. A roster
  directory whose name the channels API cannot address at all is skipped with a
  warning rather than making every global delete a permanent `500`; a malformed
  channels file stays fail-closed.
- `GET /api/agents/{name}/channels` reports `inert_overlays` — the stored
  overlays that resolve against no declared global channel and are therefore
  dropped from `bindings`
  ([#8187](https://github.com/bobmatnyc/trusty-tools/issues/8187)). A
  whole-list save now carries those records through instead of deleting a
  binding the client never saw; a submitted binding reusing an inert id
  replaces it.
- `GET /api/agents/{name}/stores` reports a `search_slot` object — the declared
  primary index, the index an unqualified `vector_search` actually answers
  from, the protected extraction index, and whether the two agree
  ([#7902](https://github.com/bobmatnyc/trusty-tools/issues/7902)). A slot that
  cannot be resolved is reported as an `error` beside a store that still reads
  as connected, rather than staying invisible. Agreement is the three-state
  `declared_agreement` (`agrees` / `differs` / `unresolved`), so a slot the
  resolver never answered for is never reported as agreeing with — or
  differing from — the declaration.
