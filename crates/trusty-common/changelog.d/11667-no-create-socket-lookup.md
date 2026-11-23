Fixed
- `resolve_data_dir_no_create`, `daemon_socket_path_no_create`, `search_dial_socket`, `resolve_memory_dial_socket` and `events_dial_socket_path` resolve the same paths as their creating versions and create no directory, for dial-side lookups (#11667).
