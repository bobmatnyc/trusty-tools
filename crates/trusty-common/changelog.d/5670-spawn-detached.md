Added

- `daemon_guard::spawn_detached(program, args)` spawns any named binary as a detached background process with all stdio null-ed. A guard does not always boot its own binary — `tga audit` has to start `trusty-analyze`, a sibling it resolves by env override or PATH (#5670) — and the alternative was a second copy of the `Command::new(…).stdin(null)…` dance in another crate. `spawn_current_exe` is now that call with `current_exe()` resolved first, so the detached-spawn capability keeps exactly one implementation.
