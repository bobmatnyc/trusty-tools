Changed
- The embedder init retry now comes from `trusty_common::embedder::retry_transient_init`, so this crate requires trusty-common 0.67.5 or later (#10662).
- Embedder init now also retries HTTP 500, a `ureq` timeout, a failed DNS lookup or unresolved host, an unreachable network, no route to host, a failed or refused connection and a stalled body read, up to 5 attempts in total. These errors failed on the first attempt before (#10662).
