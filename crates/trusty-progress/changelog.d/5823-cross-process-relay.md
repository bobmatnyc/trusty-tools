Added

- `relay` — a one-line-per-event wire format for carrying progress out of a
  child process and back into a parent's display. `StageEvent::encode` /
  `StageEvent::decode`, the `TRUSTY_PROGRESS_RELAY` opt-in variable, and the
  escaping that keeps a multi-line failure reason from breaking the framing or
  forging a second event. Both ends name this module rather than each spelling
  the grammar ([#5823](https://github.com/bobmatnyc/trusty-tools/issues/5823))
