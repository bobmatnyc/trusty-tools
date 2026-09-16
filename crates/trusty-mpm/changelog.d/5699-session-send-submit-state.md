Fixed
- `session_send` reports the submit state a post-send pane read establishes instead of a `sent: true` that could not fail: a message the harness collapsed into a bracketed paste and never submitted now returns `sent: false` with `submit_state: "unsubmitted_paste"`, and an unreadable pane returns `"unverified"` rather than claiming either outcome (#5699).
