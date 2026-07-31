Fixed

- **The TUI's initial session-events connect could hang forever, bypassing the
  reconnect budget (#3494).** `build_http_client()` sets `connect_timeout(5s)`,
  which bounds only the TCP handshake — a daemon that accepts the connection but
  never sends response headers (wedged, deadlocked, or slow-lorising) left
  `pump_session_events`'s `GET /sessions/{id}/events` `.send().await` pending
  indefinitely. `SESSION_STREAM_MAX_RECONNECTS` never applied, because that
  budget only starts counting once `.send().await` resolves. A per-request
  `.timeout(CONNECT_TIMEOUT)` (10s) now bounds connect-through-headers only; the
  body read stays governed by `SSE_IDLE_TIMEOUT`. A timed-out connect surfaces as
  an ordinary transport error, so it counts against the reconnect budget like
  every other failure rather than being a silent-hang path of its own.

- **`TurnCapExceeded` permanently un-resumed a session (#3888).** A
  session whose `task.run` call exhausted its per-call
  `AgentLoopConfig::max_turns` budget fell through
  `task::executor::run_and_record`'s terminal-status match into
  `SessionStatus::Failed`, which `SessionRegistry::begin_execution`
  permanently rejects — directly regressing epic #2343's infinite-sessions
  goal. Added a new resumable `SessionStatus::TurnCapExceeded` status
  (wire value `turn_cap_exceeded`), mapped `AgentLoopError::TurnCapExceeded`
  onto it instead of the `Failed` catch-all, and extended
  `begin_execution`'s resume condition to accept it exactly like
  `Finished` — a capped session's PM transcript (already persisted
  unconditionally) now continues on its next `task.run`.
- **`TCODE_TELEMETRY_DATA_DIR` cross-test race closed via dependency
  injection (issue #3902).** Several `agent_loop` loop-integration tests
  (`budget_tests::cadence_emits_context_budget_snapshot`,
  `tests::cadence::cadence_fires_in_daily_driver_when_configured`,
  `tests::cadence::cadence_fire_logs_info`,
  `tests::cadence::cadence_none_threshold_fire_does_not_log_error`,
  `tests::daily_driver_mode_compacts_long_running_loop`) triggered a real
  cadence or threshold-compaction fire without isolating
  `telemetry::default_data_dir()`'s process-global `TCODE_TELEMETRY_DATA_DIR`
  env var behind `telemetry::DATA_DIR_ENV_LOCK`. Under `cargo test`'s
  parallel scheduling, one of these un-isolated fires could land its
  telemetry write inside a DIFFERENT, properly-isolated test's temp
  directory — reproduced locally at ~5% (2/40) under default parallelism,
  matching PR #3896 CI's `cadence_disabled_writes_no_cadence_telemetry`
  failure exactly (same spurious `tcode-cadence` record). New
  `AgentLoopConfig::telemetry_data_dir` field lets a caller inject this
  loop's own write target directly instead of relying on the shared env
  var; every loop-integration test that needs isolation now sets it, so
  concurrent test runs can no longer race each other over telemetry writes.
  Production behaviour is unchanged (`None` falls back to
  `telemetry::default_data_dir()` exactly as before). A `#[cfg(test)]`-only
  guard in `AgentLoop::telemetry_data_dir()` now panics on the FIRST run of
  any FUTURE test that reaches a real telemetry write with neither
  `telemetry_data_dir` injected nor the env var set, turning the next
  instance of this omission into a deterministic failure instead of a ~5%
  flake — this guard immediately caught a genuinely new instance of the
  same omission in `task::executor::tests::spawn_task_run_turn_cap_exceeded_is_resumable`
  (landed on `main` via the unrelated #3898, concurrently with this fix),
  which is now closed the same way: `TaskRunParams` gains a
  `telemetry_data_dir` field (`None` on the one production call site,
  `task::protocol::task_run`) threaded into `run_and_record`'s
  `AgentLoopConfig`, and the shared `executor_tests.rs::params()` test
  helper now injects an isolated dir unconditionally so no future test
  built on it can reintroduce the omission either.

- **Test-only ambient-daemon leaks closed for two `run_task` tests (issue
  #2914).** `spawns_indexing_thread_for_non_git_project_path` and
  `background_indexing_invokes_readiness_observer` call
  `ensure_project_indexed_in_background` directly, bypassing the
  `execute_run_task` wrapper's `isolate_ambient_daemons()` guard (#3361) — on
  a machine with a live trusty-search daemon they registered their tempdir
  fixture against it for real. Both now install the same isolation guard.
  Production behaviour (task-start indexing still opts in to
  `allow_sensitive_path: true` for its own working project, issue #2747) is
  unchanged — see `trusty-common`'s changelog for the shared-helper fix this
  pairs with.
