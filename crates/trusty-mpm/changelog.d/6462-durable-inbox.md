Changed

- The peer bus's delivery boundary moved from the instance to the client
  ([#6462](https://github.com/bobmatnyc/trusty-tools/issues/6462), DOC-60 §7).
  Every subscriber of one instance used to share a single 64-slot broadcast
  ring, which is why #4271's fix had to refuse the publish to stay truthful: one
  client that stopped reading made `POST /api/v1/bus/publish` (and
  `mpm.bus.publish`) answer `503` for every publisher and every healthy
  co-subscriber, until that client drained, disconnected, or the instance was
  deregistered. **That instance-wide wedge is gone.** Each subscription now has
  its own 64-envelope inbox, so a stalled client falls behind alone: publishes
  are accepted, co-subscribers keep receiving, and the stalled client's own
  inbox displaces its oldest unread envelope to make room.
- `BusError::SubscriberLagged` and the `503` / `CODE_UNAVAILABLE` answer are
  retired from the publish path — a slow subscriber is no longer a publish
  failure, so there is nothing for a sender to retry. The `400` / `403` / `404`
  / `409` / `410` statuses are unchanged.
- Every eviction is recorded. The DOC-60 §9 durable stream now carries a second
  record shape alongside the envelope records — `{"record":"inbox_eviction",…}`,
  naming the displaced `message_id`, the `instance_id`, the `subscription_id`
  that lost it, and that subscription's running loss count. Readers of
  `logs/bus/*.jsonl` must tolerate it: an envelope line has no `record` key, an
  eviction line always does. Nothing recorded `delivered` is lost without a
  matching eviction record.
- `GET /api/v1/bus/subscribe/{instance_id}`'s `event: lagged` frame now also
  carries `subscription_id`, which is the key to find that client's eviction
  records in the durable log. Recovery is unchanged: re-read the log named in
  the frame from your last known `message_id`.
- A dropped subscription detaches itself, so a client that disconnects stops
  being fanned out to and stops holding envelopes nobody will read. A client
  that never polls costs its instance one bounded buffer and nothing else, which
  is what bounds the wedge without a daemon-side timer.
