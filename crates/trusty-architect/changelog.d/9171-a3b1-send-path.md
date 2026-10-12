Added

- `poller::Waker::wake` and `poller::dispatch_wake`, the Rust port of the poller's wake: the pointer is typed into the Architect's input box only when the pane's live session is `tm-architect` and the box reads `empty` or `suggestion`; Enter goes out only when the box text equals the pointer, and cleanup erases only the poller's own trailing text, never into an empty box; the owner is re-read before every send, and the pending count clears only on a result starting `sent` ([#9171](https://github.com/bobmatnyc/trusty-tools/issues/9171))
- `Clock` (`SystemClock`, `FakeClock`) and `ScriptedTmux`, a tmux fake that answers each verb from a script and records every argv ([#9171](https://github.com/bobmatnyc/trusty-tools/issues/9171))
