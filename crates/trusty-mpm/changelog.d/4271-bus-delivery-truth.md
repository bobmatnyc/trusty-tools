Fixed

- The peer bus no longer records an envelope as `delivered` when a lagging
  subscriber never saw it
  ([#4271](https://github.com/bobmatnyc/trusty-tools/issues/4271)).
  `broadcast::Sender::send` answers `Ok` for a receiver that is attached but
  behind, so publishing into a full 64-deep instance channel overwrote an unread
  envelope while the DOC-60 §9 durable log wrote `delivery_state: "delivered"`
  for the one that displaced it — the sender saw `202 Accepted` and the
  recipient was never told. `PeerBus::publish` now checks the channel before
  sending and refuses the new message with `SubscriberLagged` (`503`, or
  `CODE_UNAVAILABLE` over the socket), recording it `dropped`. What the log
  calls delivered is now exactly what the subscriber receives.

  Operationally this trades a silent loss for a visible refusal, and the
  refusal is per-instance: a subscriber that stops draining makes every publish
  to its instance answer `503` — healthy co-subscribers included — until it
  drains, disconnects, or the instance is deregistered. `broadcast` offers no
  way to evict one subscriber without dropping the channel, and dropping it
  would discard envelopes the log has already recorded delivered; the
  per-subscriber buffer that would bound this is DOC-60 §7's durable inbox,
  deferred. Senders should retry with backoff, and an operator seeing repeated
  `503`s should look at the recipient, which the `warn!` names.
- `GET /api/v1/bus/subscribe/{instance_id}` reports a lag instead of swallowing
  it. The handler mapped `Lagged(n)` to `None`, borrowing an idiom from the
  session SSE handlers that carry load-sheddable telemetry — which DOC-60 §3
  keeps off this bus. It now emits an `event: lagged` frame carrying the missed
  count and the durable log's path to re-read from, plus a `warn!`.
- The bus's durable JSONL stream no longer interleaves two records into an
  unparseable line when two publishes land at once. `AuditLogger::try_log` wrote
  the record and its newline as separate unbuffered writes; it now appends both
  in one `write_all`.
