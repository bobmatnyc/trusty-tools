Fixed
- `CodeEngine` now reports a cancel to the TUI as a typed `CancelReply`, so `-32010 cancel_unconfirmed` reaches the pane as a "still cancelling" state rather than a failure, and any other refusal reaches it as the daemon's own sentence with the JSON-RPC code stripped (#8207).
