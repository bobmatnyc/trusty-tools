Fixed
- `FastEmbedder` no longer loads the other model when the primary model's init fails on an error the classifier treats as transient: a held hf-hub blob lock, HTTP 429, 500, 502, 503 or 504, a timeout, or a reset, refused or failed connection, an unresolved host, or a stalled body read. It makes up to 5 attempts, then fails with a named `embedder init failed` error (#10662).
- An offline host with only the INT8 model cached now fails with that error after its attempts instead of loading INT8. If connects hang rather than fail fast, a daemon's `TRUSTY_EMBEDDER_INIT_TIMEOUT_SECS` bound (default 180 s) can fire first (#10662).
- Any other init error, including HTTP 401, 403 and 404 and a CoreML hang, is not retried and takes the same fallback path as before (#10662).
