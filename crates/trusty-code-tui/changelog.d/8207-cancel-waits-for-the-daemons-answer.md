Fixed
- Cancelling a turn now shows `cancelling…` until the backend confirms the run has stopped, instead of printing `cancelled` and reopening input on request. A prompt typed during that window queues in the composer exactly as it does during a live turn, so it can no longer reach a daemon that still has a task running — the `-32003 already has a task running` error the operator used to see (#8207).
- A backend that answers "accepted, but not stopped yet" renders as a plain `still cancelling` sentence and keeps input closed; a cancel that fails to land renders as `cancel failed` and never claims the run stopped. No cancel path prints a JSON-RPC code.
- `TuiEngine` gains `cancel_session_reply`, defaulted over `cancel_session`, so an engine with a one-state cancel needs no change.
