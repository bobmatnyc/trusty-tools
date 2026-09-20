Fixed
- The managed-session reaper now skips a session whose resume is in flight. A sweep whose live-session snapshot predated the resume's tmux recreate stopped the record, killed the pane the resume had just made, and stamped it `Deliberate`, which no automatic path revives (#8233).
- The pre-launch pane handshake reads the tri-state tmux existence probe, so a transient probe failure is logged and reported as unobservable instead of being silently read as "no session" and skipping the wedged-pane check (#8233).
