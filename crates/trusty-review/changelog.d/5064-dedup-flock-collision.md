Fixed

- `dedup.redb` no longer locks out sibling processes, and a failed claim no
  longer posts an ungated review. The dedup claim store held redb's exclusive
  file lock for the whole process lifetime, so a second opener against the same
  `--log-dir` — `serve --stdio` alongside `serve`, or an ADR-0034
  console-spawned webhook worker alongside the HTTP daemon — got
  `DatabaseAlreadyOpen`, which was downgraded to a warning and run past. The
  store now opens redb per operation and releases it again, waiting out a
  concurrent holder for up to 2s and returning a typed `DedupError::Contended`
  if it never frees; the #702 rebuild-on-unreadable-file recovery is serialised
  behind a lock so two processes cannot rename away each other's database. Every
  `AppState` builder now declares `DedupNeed`: modes that never post do not touch
  the file, and modes that can post fail to start without the claim gate. A
  claim that cannot be established aborts the review instead of proceeding. The
  review is then lost until a human re-requests it — the webhook acks with 202
  before the review runs, so GitHub does not redeliver — which is still better
  than a duplicate comment nobody can retract. An abort releases only a claim
  the aborting review actually acquired, so a failed claim never deletes
  another process's record.
  Store operations run on a blocking thread so the lock wait cannot stall a
  tokio worker (#5064).
