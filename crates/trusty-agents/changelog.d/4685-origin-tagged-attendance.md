Changed

- **Attendance turn origin is now caller-declared, and REPL slash commands
  record** (closes
  [#4685](https://github.com/bobmatnyc/trusty-tools/issues/4685); owner
  decision **origin-tagged**, 2026-08-03; part of epic
  [#4646](https://github.com/bobmatnyc/trusty-tools/issues/4646)). Two halves,
  and the first is what makes the second safe to ship.

  **`TurnOrigin` moved from inside the helpers onto their signatures.**
  `note_human_turn`, `note_human_turn_in` and `note_command_turn_in` each
  hardcoded `TurnOrigin::Human` internally, so "automation cannot forge
  presence" was a property of *which wrapper a caller happened to reach for* —
  not a property at all, since a future automated caller picking the convenient
  function forges presence silently and nothing objects. The helpers are now
  `attendance::note_turn(instance, origin)`,
  `note_turn_in(root, instance, origin, now)` and
  `note_command_turn_in(root, instance, origin, paired, now)`; the names no
  longer assert an origin the caller did not choose, and the compiler makes
  every new call site name a variant. It is now impossible to record a human
  turn without some call site naming `TurnOrigin::Human` in its own source, and
  an automated caller naming `TurnOrigin::Assistant` records nothing even with
  every transport gate open — pinned by
  `an_assistant_origin_caller_records_nothing`. `note_command_turn_in`'s two
  gates are deliberately independent: `paired` answers "is this sender entitled
  to assert presence", `origin` answers "was this a person at all", and no
  transport-level check can answer the second.

- **REPL slash commands now record a human turn** — the last gap left by
  [#4652](https://github.com/bobmatnyc/trusty-tools/issues/4652).
  `repl::commands::dispatch::try_handle_slash` is a separate dispatch table
  from `repl::dispatch::attempt_forward` (the only REPL site #4652 hooked), so
  every recognized slash command — `/switch`, `/model`, `/provider`, `/status`,
  `/help` — returned from it and never reached the hook. An owner polling
  `/status` while a long task ran read as absent after fifteen minutes while
  sitting right there. `try_handle_slash` takes an explicit `origin` and records
  through it; pinned by `repl_slash_command_records_a_human_turn`, which drives
  the real dispatcher against an injected attendance root rather than a helper
  in isolation — the defect being fixed was precisely a dispatcher nobody had
  wired, which a helper-only test would have passed throughout.

  The REPL has no pairing or RBAC gate, and unlike Telegram and Slack there is
  nothing to hang one on: there is no remote sender to authenticate, because the
  process already runs under the operator's own uid. **`TurnOrigin` is the
  REPL's gate**, and it is load-bearing rather than ceremonial —
  `try_handle_slash` has non-human callers today.
  `ctrl::repl::plain_cli::run_plain_cli` issues its own `/switch assistant` at
  startup before reading a byte of stdin, and
  `runtime::predispatch::handle_slash_passthrough` serves `tagent /status` from
  argv, a surface documented for scripting; both now pass
  `TurnOrigin::Assistant`. An unconditional hook would have made every REPL
  launch — and every cron-driven `tagent /status` — forge attendance for an
  absent owner. `an_automated_repl_slash_caller_records_nothing` pins that, with
  a human-origin contrast so it fails against an unhooked dispatcher rather than
  passing vacuously.

  The TUI's `ReplBridge::handle_input` records too, because it intercepts and
  returns from six commands (`/exit`, `/clear`, `/switch`, `/model`,
  `/provider`, `/update`) before `try_handle_slash` ever sees them — the same
  "two dispatch tables drift apart" shape this issue exists to fix. The hook
  sits at the one point every submitted line passes through; the later
  `try_handle_slash` call is idempotent under the existing monotonic guard.
