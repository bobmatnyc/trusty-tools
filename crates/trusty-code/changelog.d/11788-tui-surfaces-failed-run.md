Fixed
- `tcode tui` no longer ends a failed turn in silence: the run's error-level log line (for example `PM config error: ...`) shows in the chat, and a run that ends with status `failed` adds a visible error line (#11788).
