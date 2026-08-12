Changed

- `events::EVENT_LINE_PREFIX` is re-exported from
  `trusty_agents_common::events` rather than declared here. The emitted value is
  unchanged (`__OMPM_EVENT__ `); the second copy is what let the session
  manager's instructions drift onto a marker nothing emits
  ([#5129](https://github.com/bobmatnyc/trusty-tools/issues/5129)).
- `events::format_event_line` is now public, so the exact line `emit` writes to
  stderr is testable without capturing real stderr.
