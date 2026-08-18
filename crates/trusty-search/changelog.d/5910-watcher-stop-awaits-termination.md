Fixed

- `DELETE /indexes/{id}` no longer releases its teardown guard while the index's
  file watcher still holds the indexer. `WatcherTask::stop` only called
  `JoinHandle::abort()`, which reaps an idle task inline but not a running one,
  so the watcher's `Arc<RwLock<CodeIndexer>>` — and the open redb corpus it owns
  — could outlive the delete. Recreating the same id then hit
  `DatabaseAlreadyOpen`, set `corpus_open_failed`, and answered `500`. `stop`
  now awaits the consumer task's termination (issue #3049).
