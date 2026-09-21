Fixed
- Hold the in-flight resume claim across the WHOLE operator resume — the self-heal, the prompt refresh, the pane handshake, the spawn and the post-send check — so the runtime reaper can no longer stop a session mid-resume and hand the next supervisor tick a second launch; a second resume arriving meanwhile is refused as "already resuming" (409), not reported as a launch failure (#8233).
- Count a failed auto-resume once: the supervisor no longer appends a second `[error: ...]` note to a record `resume_auto` already marked errored (#8233).
- Treat a pane whose tmux session cannot be observed as unknown rather than unresponsive, so a launch is no longer refused after the full blocking budget on the documented tmux-absent fallback (#8233).
- Stop parking a Tokio worker for up to 3 s during the pre-launch pane handshake, on the same runtime that serves the daemon's HTTP routes (#8233).
- Log, rather than discard, a store failure while recording a launch that never came up (#8233).
- Strip an `[error: ...]` note whose message contains its own `]` completely, instead of leaving its tail on the task forever (#8233).
