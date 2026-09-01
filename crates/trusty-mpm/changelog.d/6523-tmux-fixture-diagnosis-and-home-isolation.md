Fixed

- The `full_user_cycle` lifecycle test no longer writes a real
  `~/.trusty-mpm/sessions/<uuid>/pause.json` into the operator's home on every
  run. It redirects `$HOME` to a scratch directory, and takes
  `host_state_gate`'s `TRUSTY_MPM_ALLOW_HOST_STATE` opt-in so the scratch
  `$HOME` does not silently void its real-tmux assertion (#6523).
- A failed `ScratchTmuxSession::spawn` now quotes tmux's own stderr in the
  panic. The exit status alone did not distinguish a duplicate session name
  from a host that has run out of pseudo-terminals, so both read
  `exit status: 1` (#6523).
- `drop_kills_the_session_even_when_the_body_panics` no longer passes
  vacuously when the fixture cannot create a tmux session: the spawn moved out
  of the `catch_unwind` that was swallowing its failure (#6523).
