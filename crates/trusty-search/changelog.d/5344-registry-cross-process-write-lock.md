Fixed

- `indexes.toml` writes now take a cross-process advisory lock, not just the
  process-wide mutex added in
  [#5335](https://github.com/bobmatnyc/trusty-tools/pull/5335)
  ([#5344](https://github.com/bobmatnyc/trusty-tools/issues/5344)). `prune`,
  `prune-orphans` and `migrate storage` run as separate processes from the
  daemon, so nothing ordered their whole-file overwrites against its writes —
  a session on 2026-08-05 observed five daemons against one registry with
  last-writer-wins. Every write now blocks on an `indexes.toml.lock` sidecar
  via the shared `trusty_common::file_lock` entry point.
- `prune`, `prune-orphans` and `migrate storage` no longer republish the
  survivors of the snapshot they loaded. They remove or patch BY ID, re-reading
  the current file under the lock, so an index registered while the operator was
  reading the report — or while the migration was copying data directories — is
  no longer deleted with the command reporting success.
