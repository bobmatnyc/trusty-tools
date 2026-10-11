Added
- `palace_verify_embedded` answers `chunk_check`, `chunk_check_error`, `embedder_ready`, and `embedder_state` (`ready`, `warming` or `failed`) with `embedder_error` naming a failed startup warm-up, so a caller can tell a warm-up in progress from one that failed (#9275).
