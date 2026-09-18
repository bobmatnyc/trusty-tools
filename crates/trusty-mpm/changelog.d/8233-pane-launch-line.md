Fixed
- Managed launches now reset the pane to a primary prompt before typing and confirm through a `<session>.started` sentinel that the shell actually ran the launch line, so a pane wedged at a continuation prompt is named as the cause within ~2 s instead of timing out as a generic "no runtime came up" (#8233).
- Abandoned launch specs — left by a killed pane, a daemon restart or a launch the shell never ran — are reaped after ten minutes, so the `GH_TOKEN` and `CLAUDE_CODE_OAUTH_TOKEN` they carry no longer sit on disk indefinitely (#8233).
- The `tcode` adapter routes its `run-task` line through the pane-command length guard; a task long enough to overflow the tty's canonical buffer is now refused instead of silently truncated into a different task (#8233).
- A bare-`tm` in-place relaunch resolves and applies the project's pinned `gh` identity itself, restoring the `GH_TOKEN`/`GH_CONFIG_DIR` binding the pane shell stopped carrying once the launch moved into a spec file (#6668, #8233).
- The launch-on-main spawn path verifies its launch like the other two spawn sites, so a shim failure errors the record in seconds instead of leaving it Active until the reaper (#8233).
