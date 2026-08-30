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
- `GET /api/v1/bus/subscribe/{instance_id}` reports a lag instead of swallowing
  it. The handler mapped `Lagged(n)` to `None`, borrowing an idiom from the
  session SSE handlers that carry load-sheddable telemetry — which DOC-60 §3
  keeps off this bus. It now emits an `event: lagged` frame carrying the missed
  count and the durable log's path to re-read from, plus a `warn!`.
