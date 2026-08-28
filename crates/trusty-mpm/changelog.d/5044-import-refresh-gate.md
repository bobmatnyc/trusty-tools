Added

- `tm memory import --refresh` (#5044) replaces a drifted drawer with the
  file's current text — `memory_forget` then `memory_remember`, since
  trusty-memory has no update-in-place tool — instead of reporting the drift
  and leaving the stale copy in the palace. The plain run is unchanged and
  still refuses to touch a drifted drawer; its skip reason now names
  `--refresh`. Two new report statuses, `refreshed` and `would-refresh`, and a
  `refreshed` count in the JSON report and the summary line.
- Under `--refresh`, every file whose drawer the run names is put through
  `palace_verify_embedded` (#6328) and fails unless that drawer is retrievable
  right now. The `#4834` flow deletes source files on a clean exit, so a drawer
  that is stored but absent from vector search — or a gate that could not run
  at all — blocks the run rather than passing it. `--dry-run --refresh` reports
  the same verdicts without writing.
- A refresh that removes the stale drawer and then fails to write its
  replacement reports `DATA LOSS`, names the removed drawer, and says not to
  delete the source. A refresh whose `memory_forget` fails reports that nothing
  changed. The two arms never read alike.
