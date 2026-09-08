Fixed

- Every tm-managed Claude Code launch (`tm launch`, `tm connect`, `tm run`, in-place resume/relaunch) now also provisions `CLAUDE_CODE_DISABLE_MOUSE=1` alongside `CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN=1`, so tmux's per-pane `mouse_any_flag` no longer captures the wheel under the classic renderer. Yields to an operator-exported value, decided independently per variable (#7160).
