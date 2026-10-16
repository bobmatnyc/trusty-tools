Removed
- The crate's UI pipeline: `build.rs`, the committed `ui-dist/` bundle, and the Makefile's `build-ui`, `sync-ui`, `release-prep` and `ui` targets. The published tarball no longer carries either file; the search dashboard is built, served and published by trusty-console (#9535).
- `SearchAppState::daemon_port`, `SearchAppState::openrouter_enabled` and `SearchAppState::with_daemon_port`, which had no reader once the `/ui` handlers were deleted. `with_openrouter_api_key` still sets the key that chat-provider detection reads (#9535).
