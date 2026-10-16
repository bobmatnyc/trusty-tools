Fixed
- `tm sessctl connect` takes the session's write-lock again, now over the daemon socket: the first open connect is the writer and a later one observes, as on the old SSE route. The lock is released when the stream ends — the client leaving (even on a quiet session), the session ending, or a failed send (#6288).
