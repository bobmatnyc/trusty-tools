Fixed
- `tcode tui` bounds opening a daemon stream (`session.events`, `workstream.events`) by the 15-second call budget instead of the 15-minute frame budget, so a daemon that stops reading the request no longer holds the client for fifteen minutes (#7216).
