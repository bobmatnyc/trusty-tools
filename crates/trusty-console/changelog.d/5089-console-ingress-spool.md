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
- the route accepts bodies up to 25 MiB. axum's 2 MiB `DefaultBodyLimit` would
  413 a real `push` / `pull_request` delivery *before* the handler runs — no
  spool entry, no metric, no log — which is the same invisible drop arriving
  through the framework instead of the code
- `GET /api/console/metrics/webhooks` — oldest-pending-age, pending count and
  failed-attempt total as a standard `ConsoleMetricsReport`, red once the
  oldest entry passes the threshold. The scan runs on the request rather than
  from a cache, so the signal does not go quiet if the background retry sweep
  stops. A spool directory that cannot be read — including one that was opened
  and has since been removed or unmounted — is red, never an empty listing
- retries are claimed and backed off. A `ClaimSet` gives one relay per entry at
  a time, so a sweep tick landing inside the request path's own relay window
  cannot send the same delivery twice; `BackoffPolicy` spaces attempts
  exponentially (30 s doubling to a 1 h ceiling) and stops at 24 failures. An
  exhausted entry is never relayed again and never deleted — it holds the
  health signal red until an operator intervenes
- a fresh entry is committed with `hard_link`, not `rename`, so a colliding
  path fails atomically instead of clobbering a delivery that may already have
  been acknowledged
