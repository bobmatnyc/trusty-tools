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
  turn never rewinds the clock.

  Recorded at the four surfaces a person can actually address an assistant
  through, each against the persona the message named: `POST /api/task`, the
  REPL's `attempt_forward` (one call covering all six of its dispatch arms),
  Telegram past the pairing gate, and Slack past the pairing and RBAC gates. The
  hook is infallible by construction — an unusable instance name, a missing home
  directory or an I/O error is logged at debug and swallowed, because attendance
  is a hint for #4653 and never a reason to fail a turn a user is waiting on.

  Signal only: nothing here sends or queues, and `trusty-agents` gains no
  dependency on `trusty-mpm`.
