Fixed
- `session_send` types the text, waits up to 2 s for the input box to show it (or a `[Pasted text #N]` placeholder), and only then presses Enter. When the box shows a dropped head it clears the box and returns `sent: false` with `submit_state: "not_echoed"` and a `reason`, instead of submitting the tail (#10747).
