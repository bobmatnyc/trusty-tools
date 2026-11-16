Added
- `send_line_guarded` types one line through `send_guarded` and presses Enter only after the line echoes in the box; otherwise it returns the new `SendError::NotEchoed` (#11365).
