Fixed

- `service install` no longer destroys environment variables the live unit
  carried. Before this issue, install wrote a differently-named plist and never
  touched the running agent, so anything the template failed to reproduce was
  merely absent from a file nobody read. Once install began overwriting the LIVE
  unit, the same gap became data loss: the plist on the owner's host carried
  `TRUSTY_WARMBOOT_INDEX_TIMEOUT_SECS`, `TRUSTY_EMBEDDERD_CALL_TIMEOUT_SECS`,
  `FASTEMBED_CACHE_DIR`, `FASTEMBED_CACHE_PATH` and `RUST_LOG`, none of which the
  named-tunable allowlist mentioned. The first is the hand-patch from an incident
  where a restart cost a 200k-chunk index to a 30 s redb open timeout under
  warm-boot contention — dropping it re-arms that incident, invisibly, until the
  next restart. Every key the installed unit carried is now carried forward
  unless the template computes it itself (`HF_HOME`, `PATH`), so extending an
  allowlist is no longer what stands between an operator and a lost setting
  (#4868)
- The regenerated unit keeps a `WorkingDirectory` the installed one had. The
  template never emitted the key, so regeneration silently changed the daemon's
  working directory (#4868)
- `make deploy` boots out the LEGACY label as well as the canonical one. On a
  mid-migration host the loaded unit is still `com.trusty.trusty-search` with
  `KeepAlive::Always`, so booting out only the canonical label left that daemon
  running into the binary swap — the #87 hazard the bootout exists to prevent,
  and which the old `unload $(PLIST_LEGACY)` line had covered (#4868)
