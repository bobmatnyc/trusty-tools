Fixed
- Sessions lost with a tmux server stay auto-resumable after a later resume starts a new server; the reaper compares each session's stored server identity with the live server's instead of stamping them Deliberate (#10700).
