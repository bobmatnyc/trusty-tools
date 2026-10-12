Fixed
- A stream-json worker that exits non-zero or is killed by a signal now ends its session `Failed`, with the exit code or signal in the failure reason, instead of `Stopped (completed)`. A worker that exits 0 still completes. A worker that closes its output but keeps running is killed and reaped after 5 seconds and the session fails (#9725).
