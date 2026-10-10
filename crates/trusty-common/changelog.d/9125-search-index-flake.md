Fixed
- The two `search_index` hung-up-create tests no longer fail under full-suite load. Their mock daemon now drops the connection without running the process-global panic hook, which could stall the mock's only thread past the registry-confirm deadline (#9125). Test-only; no runtime behavior changes.
