Added

- **`attendance` — "is a human currently attending this assistant instance?"**
  (closes [#4652](https://github.com/bobmatnyc/trusty-tools/issues/4652); part
  of epic [#4646](https://github.com/bobmatnyc/trusty-tools/issues/4646), owner
  decision **D3**). Nothing could answer that question, and the `notify_owner`
  tool ([#4653](https://github.com/bobmatnyc/trusty-tools/issues/4653)) cannot
  exist without an answer. The new module exposes `AttendanceTracker`,
  `Attendance` and `is_unattended()` for #4653 to gate on. Per D3 the mechanism
  is a last-human-turn timeout with one tunable threshold — explicitly not SSE
  connection counting and not a manual do-not-disturb toggle.

  This is new work rather than a reuse of `SessionRecord.last_activity_at`
  because that field advances on the assistant's own tool and hook activity
  exactly as it does on a human typing, so an assistant grinding through a long
  solo task reads as attended forever. Here the origin of a turn is an explicit
  `TurnOrigin` argument and only `TurnOrigin::Human` advances the clock;
  assistant activity writes nothing at all, which
  `assistant_turns_never_advance_the_clock` and
  `assistant_only_activity_leaves_no_record_at_all` pin. `Attendance` carries a
  distinct `NeverAttended` state — no human turn has ever been recorded — which
  counts as unattended, and the threshold boundary is inclusive on the
  unattended side, so "N minutes" means N minutes of silence is enough. A
  timestamp in the future (clock skew) reads as attended, biasing toward silence
  rather than toward interrupting someone who is present.

  **The threshold defaults to 15 minutes**, overridable with
  `TAGENT_UNATTENDED_AFTER_MINS` (whole minutes; blank, non-numeric or zero
  input keeps the default rather than failing a boot). Long enough to clear an
  ordinary interruption, short enough that a real walk-away is noticed inside
  one break.

  **Durable**, and that is correctness rather than tidiness: an in-memory
  tracker would report a fresh process as never-attended, so restarting the API
  server mid-conversation would hand #4653 a licence to notify a human who is
  demonstrably present. State is one small JSON record per instance under
  `~/.trusty-agents/attendance/`, written through the existing lock+tmp+rename
  `state_writer` so a GUI, an `--api` sidecar and a REPL sharing that tree
  cannot tear each other's records. Recording is monotonic — an out-of-order
  turn never rewinds the clock — and the guard is evaluated under the SAME held
  lock as the write (`state_writer::atomic_update`, new in this change), so two
  processes recording near-simultaneous turns cannot both read the old value and
  let the loser's older write land last. Pinned by
  `concurrent_writers_never_rewind_the_clock`, which fails against a
  read-then-write guard.

  Recorded at every surface a person can actually address an assistant through,
  each against the persona the message named: `POST /api/task`, the REPL's
  `attempt_forward` (one call covering all six of its dispatch arms), and BOTH
  inbound paths on Telegram and Slack — free text and slash commands alike. The
  hook is infallible by construction — an unusable instance name, a missing home
  directory or an I/O error is logged at debug and swallowed, because attendance
  is a hint for #4653 and never a reason to fail a turn a user is waiting on.

  Signal only: nothing here sends or queues, and `trusty-agents` gains no
  dependency on `trusty-mpm`.

  Three attendance gaps were caught in review of
  [#4683](https://github.com/bobmatnyc/trusty-tools/pull/4683), all the same
  shape — a live entry point that never advanced the clock, so a human who was
  demonstrably present read as unattended after fifteen minutes:

  - Telegram's `/switch <persona>` intercept returns early from
    `handle_message`, so a paired human switching persona was never recorded.
    The hook now runs before the intercept, fused with the routing decision so
    the two cannot drift apart again — pinned by
    `switch_command_still_records_a_human_turn`.
  - Telegram's `handle_command` (`/start`, `/pair`, `/help`, `/connect`,
    `/clear`, `/status`) and Slack's (`/slack-start`, `/slack-pair`,
    `/slack-connect`, `/slack-clear`, `/slack-switch`, `/slack-status`)
    recorded nothing at all, so a human polling `/status` every few minutes
    while a long task ran was invisible. Both now record through one shared
    `attendance::note_command_turn_in`, behind the same authentication each
    transport's message path already records behind: paired on Telegram, paired
    AND a known RBAC identity on Slack — so an unpaired chat, or an unknown
    Slack user posting in a paired channel, still cannot manufacture attendance
    for someone else's assistant. Pinned by
    `paired_slash_command_records_a_human_turn`,
    `unpaired_slash_command_records_nothing`,
    `unknown_rbac_user_cannot_manufacture_attendance` and, end to end,
    `a_command_only_session_stays_attended`.
