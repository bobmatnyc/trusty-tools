Changed

- Both tmux session-creation call sites (`tmux::orchestrator`, `debugger::tmux`) pass the new `alternate_screen` parameter `trusty_common::tmux::managed_session_commands` gained in [#5151](https://github.com/bobmatnyc/trusty-tools/issues/5151), using `DEFAULT_TMUX_ALTERNATE_SCREEN` (`true`). Behaviour is unchanged — `true` is tmux's factory value.
