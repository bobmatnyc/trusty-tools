Fixed

- Every attendance recording site now takes its root as an argument, closing a
  class of defect where a call site was untestable by construction
  (closes [#4703](https://github.com/bobmatnyc/trusty-tools/issues/4703)). Five
  production entry points — `POST /api/task` (`submit_task`) and the Slack and
  Telegram `handle_message` / `handle_command` handlers — recorded a human turn
  through a helper that resolved `$HOME` internally. The natural regression test
  for any of them ("call the handler, assert attendance was recorded") could
  therefore only write into the developer's real
  `~/.trusty-agents/attendance/<persona>.json`, with nothing local to assert
  against — so it passed whether or not the handler worked. PR #4695 hit exactly
  that: a first-draft test passed against known-broken code.

Removed

- `attendance::note_turn` — the `$HOME`-resolving wrapper behind that trap — is
  deleted rather than hidden behind `#[cfg(not(test))]`. A `cfg` gate would only
  close the trap for unit tests compiled into this crate; integration tests,
  doc-tests and downstream consumers all compile the library without
  `cfg(test)`, so the function would remain reachable for them. Deleting it
  removes the trap from every build configuration, and turns "I forgot to inject
  a root" from a silently-passing test into a compile error. `$HOME` resolution
  moved up to construction (`AppState::default`, `run_slack_bot`,
  `run_telegram_bot`), which stores it in a field a test can point at a tempdir.

Added

- A regression test for each of the five entry points, asserting attendance
  through a tempdir root — the tests that could not be written before. Each was
  confirmed to fail when its handler's hook is removed.
- `attendance::AttendanceRoot`, the injected-root shape the chat transports
  thread through their handlers.

Changed

- `slack::handlers::post_message` refuses to issue a request when no bot token
  is configured. `chat.postMessage` without a bearer token always answers
  `not_authed`, and the client it builds carries no timeout — so on a network
  that blackholes rather than refuses, a doomed request does not fail, it hangs,
  holding whichever handler called it.
