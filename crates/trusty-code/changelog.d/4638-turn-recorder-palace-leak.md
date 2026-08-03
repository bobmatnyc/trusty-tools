Fixed

- **Turn recorder no longer mints one memory palace per session (issue
  #4638).** `SessionRegistry::memory_sink_for` derives the recorder's palace id
  from the session's project root, so a session bound to a `tempfile::TempDir`
  produced a per-RUN-unique id (`t-tmp<random>` on macOS) and the drain task's
  `ensure_palace` step (#2424) auto-created that palace on the live
  trusty-memory daemon at the first turn. Nothing could ever read it back — the
  root it was keyed to is swept when the run ends — so the orphans accumulated
  at ~250-300/day: 5,667 in three weeks, 97.8% of every palace on the affected
  machine, which turned trusty-memory's O(n) full-registry HTTP handlers
  (#4637) into a ~90-minute `GET /api/v1/status`. The sink now carries an
  explicit `PalaceCreation` entitlement, `Allowed` only for a durable project
  root and `Forbidden` for one under a system temp root, making `palace_create`
  structurally unreachable from an ephemeral root rather than merely unlikely.
  N sessions therefore mint at most ONE palace — the project's — reused by
  every session on that project and shared with the PM catch-up digest, with
  sessions distinguished inside it by `chat_turn_append`'s `session_id` and
  `memory_remember`'s `session:<id>` tag exactly as `recall_session` (#2348)
  already expects.
  - Recording itself is unchanged: a `Forbidden` sink is still constructed (so
    the PM's `recall_session` tool surface does not change for a temp-rooted
    run) and still dual-writes into a palace that already exists — only
    bringing a NEW one into being is withheld. When its palace is absent the
    turn is dropped rather than written, since `memory_remember` against a
    missing palace fails `-32603` anyway; the probe is re-issued each turn so a
    palace created out-of-band mid-session is picked up.
  - The dominant real-world source was the crate's own test suite: dozens of
    tests across `task::executor_tests`, `task::protocol_tests`, and
    `tests/*_e2e.rs` drive sessions against `TempDir` project roots while the
    sink resolves the LIVE daemon URL, so every `cargo test -p trusty-code` on
    a machine with a running trusty-memory daemon permanently leaked palaces.
    The fix is inside the daemon rather than in test-harness env isolation, so
    it also protects an end user who binds a session to a temp directory.
  - Pre-existing `t-tmp*` palaces are untouched — this change never deletes
    anything and degrades gracefully if it encounters one (a `Forbidden` sink
    whose palace happens to exist simply writes into it).
