Fixed
- The local model server probe (`local_probe::probe_local`, `list_models`, `chat::auto_detect_local_provider`) now holds to its one-second budget even when building the HTTP client stalls, as the macOS system-proxy lookup can. The client is built off the async worker threads, and one deadline covers the build and the request; a missed deadline reports the server as unreachable (#9213).
