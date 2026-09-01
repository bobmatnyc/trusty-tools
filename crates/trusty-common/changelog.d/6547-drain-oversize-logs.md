Fixed

- `log_drain` now streams each log file instead of reading it whole, so a file
  over the old 64 MiB ceiling uploads rather than being skipped forever. The
  collector held five live copies of a body — plaintext, decoded, filtered,
  scrubbed, gzipped — which is the only reason the ceiling existed; 29 of 86
  daily-rotated daemon logs, up to 176 MB each, were permanently undrainable on
  the first live run (#6547). The new `pipeline` module reads 1 MiB at a time,
  splitting on the last line terminator, and carries the level filter's
  disposition and a scrub window across each boundary, so the streamed result
  matches the buffered one. The scrub window holds back the last
  `longest needle - 1` bytes of every chunk, which is what makes chunking safe
  for a secret that straddles a boundary (the hazard #6534 refused to chunk
  over).
- `DEFAULT_MAX_FILE_BYTES` moved from 64 MiB to 4 GiB, and a second bound,
  `DEFAULT_MAX_WIRE_BYTES` (64 MiB), caps the COMPRESSED body — the one buffer
  that still scales with the file, since `LogDestination::put` takes an
  in-memory `Bytes`. `collect` now takes a `CollectLimits` in place of a bare
  `max_file_bytes`, and `DrainConfig` gained `with_max_wire_bytes`.
- A file that does trip either bound has the decision recorded in the manifest
  as a `SkipRecord`, so it is made once per `(file, size, mtime)` rather than
  every cycle. The drain re-decided ~40 permanently-oversize files every 15
  minutes and re-logged the same warning — 1,276 of them in 48 hours. A later
  pass that sees an unchanged file counts it silently; a size or mtime change
  re-evaluates it; and any file the pass can read has its record dropped, so
  raising a bound takes effect on the next pass. `DrainReport` gained
  `skips_recorded`, the count of decisions that actually warned. `skips` is
  `#[serde(default)]`, so manifests written before this change still decode.
