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
  / `409` / `410` statuses are unchanged, and `BusError` is now
  `#[non_exhaustive]`.
- Every loss is recorded. The DOC-60 §9 durable stream now carries a second
  record shape alongside the envelope records — `{"record":"inbox_miss",…}`,
  naming the lost `message_id`, the `instance_id`, the `subscription_id` that
  lost it, and that subscription's running loss count. Readers of
  `logs/bus/*.jsonl` must tolerate it: an envelope line has no `record` key, a
  miss line always does. Nothing recorded `delivered` is lost without a matching
  miss record.
- A publish that no inbox accepted is recorded `dropped` and answered `409`,
  never `delivered`. This is reachable when a deregistration closes every
  subscription while a publisher is mid-flight.
- A subscription that attaches after its instance was deregistered ends
  immediately instead of hanging. `subscribe` resolves the instance and then
  attaches, so an attach could land after the deregistration had already closed
  every inbox present — leaving one that nothing would ever close, whose SSE
  stream stayed open on keep-alives forever. An instance's inbox set is now
  terminal once closed, under the same lock the attach takes.
- When a miss record cannot be written — the durable sink is failing, and §9's
  logger swallows that by design — the delivery's own record carries
  `losses_unrecorded: true` and an `error!` is emitted, so no line claims a
  clean delivery the stream cannot back. The field is absent in every other
  case, so an ordinary record is byte-identical to what it was.
- `GET /api/v1/bus/subscribe/{instance_id}`'s `event: lagged` frame now also
  carries `subscription_id`, which is the key to find that client's miss
  records in the durable log. Recovery is unchanged: re-read the log named in
  the frame from your last known `message_id`.
- A dropped subscription detaches itself, so a client that disconnects stops
  being fanned out to and stops holding envelopes nobody will read. A client
  that stays attached and never reads — a TCP-wedged SSE body, where the
  subscription is never dropped — costs its instance a bounded buffer plus one
  miss record and one `warn!` per subsequent publish, for as long as it stays
  attached. No publisher or co-subscriber pays for it; the operator's lever is
  deregistering the instance, and the `warn!` names which one.
