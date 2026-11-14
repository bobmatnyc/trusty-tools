Fixed
- A `backend_timeout` reply now says whether the call waited N s for the Keychain or was refused at once because an earlier call on the same item still hangs, and names the recovery step (answer the access prompt, or end the server so the next call starts a fresh one). The error data adds `waited_ms` and `abandoned_item`; the kind and code are unchanged (#11215).
- A `backend_timeout` audit row now carries `backend_wait` (`backend`, `waited_ms`, `abandoned_item`). The field is additive and omitted on every other row (#11215).
- The server's start and exit lines in `trusty-secrets.stderr.log` now begin with a UTC timestamp (#11215).
