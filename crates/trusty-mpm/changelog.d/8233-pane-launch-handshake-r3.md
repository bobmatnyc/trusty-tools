Fixed
- Confirm the pane's shell is executing what it is typed before a managed launch is sent, and recognise a shell continuation prompt (`quote>`, `cursh>` and kin) as its own state, flushed with an interrupt rather than a closing quote or a bare Enter (#8233).
- Key the launch-started sentinel on the launch instead of the session, so a marker left by an earlier launch can no longer satisfy a later one (#8233).
- Poll for the runtime before consulting the sentinel, so a slow-starting session is never interrupted on a 2 s budget; report "not delivered" apart from "ran and failed", at ERROR, so the failure reaches errors.jsonl (#8233).
- Clear a record's accumulated `[error: ...]` notes once its runtime is verified running (#8233).
- Relaunch the runtime on the automatic resume paths through the same adapter and the same verification an interactive resume uses; a record becomes Active only behind a runtime that was seen (#8233).
- Resolve a fresh spawn's own pane id, so the pre-launch interrupt no longer lands in the operator's active pane (#8233).
- Return an error from `resume_managed` after it marks a record errored, instead of reporting success (#8233).
- Route the control backend's tmux launch through the pane-command length guard (#8233).
- Sweep abandoned launch specs on daemon boot and on every supervisor sweep, not only when a later launch happens (#8233).
