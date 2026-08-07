Added

- `POST /api/webhooks/{source}` — GitHub webhook ingress that verifies the HMAC
  once over the exact received bytes, writes the delivery to an fsync'd spool
  under the console data directory, and only then acknowledges (#5089 step 3,
  ADR-0034). `{source}` multiplexes `review` and `analyze`; each relays over a
  hardened Unix socket. The ordering is the point: a spool write that fails
  returns **5xx and no 202**, so GitHub keeps the delivery redeliverable, where
  both existing handlers return 202 first and downgrade every later failure to
  a log line GitHub will never retry
- a relay outcome other than an explicit `"ack": true` leaves the spool entry
  `pending` with an incremented attempt count and a durable reason. Reaching
  the target is deliberately not enough — an entry is deleted only on the
  target's own acknowledgement
- `GET /api/console/metrics/webhooks` — oldest-pending-age, pending count and
  failed-attempt total as a standard `ConsoleMetricsReport`, red once the
  oldest entry passes the threshold. The scan runs on the request rather than
  from a cache, so the signal does not go quiet if the background retry sweep
  stops; a spool that cannot be read is red rather than empty
