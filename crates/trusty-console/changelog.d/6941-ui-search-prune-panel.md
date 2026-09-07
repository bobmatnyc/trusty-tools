Added
- The search dashboard carries a stale-registration panel at `#/indexes/cleanup`,
  reached from a Stale registrations button on the Indexes screen. It reads
  trusty-search's own `GET /registry/orphans` — which walks `indexes.toml`, so it
  is the only screen that can list a registration the warm-boot allowlist
  excluded — and deletes a confirmed batch through `DELETE /indexes/{id}`, one
  request per id pinned to the root the census reported. Roots the daemon
  declined to judge are listed, never selectable, and settled one at a time
  behind their own confirmation. Two rules are enforced in `lib/cleanup.js` and
  tested there: eligibility reads the daemon's root classification and
  `chunk_count` and never `size_bytes` / `disk_bytes`, which report `0` for a
  healthy 71,433-chunk colocated index (#4706); and a delete counts as a removal
  only when the response BODY carries `ok` and `removed`, so an id the daemon did
  not remove — a `404 removed:false`, or a `500` whose durable cleanup failed
  (#6363) — is shown as not removed rather than as success
  ([#6941](https://github.com/bobmatnyc/trusty-tools/issues/6941)).
