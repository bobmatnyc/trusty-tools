Changed

- **A session the GUI creates keeps the delegating PM (#8184).** `tcode`'s
  `session.create` now defaults to a solo agent that reads, edits and runs
  shell itself and asks before each mutating call. This GUI has no
  permission-prompt UI, so a solo session here would stall on an ask nobody
  can answer. `POST /sessions` therefore inserts `delegate: true` when the
  webview sends no `delegate` field of its own, and leaves an explicit choice
  alone so a future build can opt in once it ships a prompt surface.
