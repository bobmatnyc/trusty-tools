Documentation

- `BASE-AGENT.md`'s "Never Narrate a Wait" section now teaches `tm wait --for
  run|file|check --timeout <secs>` as the primary in-turn wait, ahead of the
  hand-rolled `sleep`/`until` poll loop, which stays documented only as the
  fallback when `tm` is not on `PATH`
  (refs [#5843](https://github.com/bobmatnyc/trusty-tools/issues/5843),
  closure condition 2). `tm wait` shipped in
  [#6235](https://github.com/bobmatnyc/trusty-tools/pull/6235) and is published
  in trusty-mpm 1.5.0.
