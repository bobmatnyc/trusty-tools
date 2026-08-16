Changed

- A second session launching on the same main checkout logs at `info` rather than `warn`, and `session_new`'s tool description no longer claims every session gets a freshly-cloned workspace. With writers isolated and the write boundary enforced, sharing a read-only checkout is expected rather than hazardous.
