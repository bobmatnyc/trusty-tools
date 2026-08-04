Fixed

- A `.ready` venv recheck that runs out of time is no longer treated as proof
  the venv is broken. The eager, once-per-daemon-start `import
  sentence_transformers` recheck ran under a flat 10 s budget while the daemon
  warm-booted hundreds of indexes, so torch's import routinely exceeded it and
  an intact venv was condemned to a full rebuild. A timeout is now a distinct
  outcome (retried once with a much larger budget, then reported as
  indeterminate and the venv reused) rather than a verification failure (#4125)
- `uv` discovery no longer looks only at the live `PATH`. `uv` lives in
  `/opt/homebrew/bin`, which launchd's minimal daemon `PATH` omits, so every
  needless rebuild hard-failed with "`uv` not found on PATH" and pinned
  trusty-search on the Rust ort embedder for the rest of the daemon's lifetime.
  Discovery now goes through `trusty_common::bin_resolve::resolve_binary`, the
  shared resolver added for the same bug class in #1298 (#4125)
