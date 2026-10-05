Added
- `trusty-secrets serve` and the default `server` feature (S2 of DOC-74 §15, [#9065](https://github.com/bobmatnyc/trusty-tools/issues/9065)): the `secrets.scopes`, `secrets.list`, `secrets.set`, `secrets.delete`, `secrets.copy` and `secrets.doctor` methods as newline-terminated JSON-RPC 2.0 on `~/.trusty-tools/trusty-secrets/secrets.sock` (parent 0700, socket 0600, every connection uid-checked).
  - The socket starts on the first call through `OnDemandSecrets` and exits after 60 s with no answered request (`TRUSTY_SECRETS_IDLE_TIMEOUT_SECS`), removing its socket file. It has no launchd job, and a second instance never replaces a live one.
  - Each request names its project by directory. The server derives owner and repository from that checkout's git remote and reads the project's `secrets:` config from `<repo>/.trusty-tools/trusty-secrets.yaml`.
  - No method returns a secret value. Every failure is fixed text per method and error kind, so a malformed `set` never echoes its value.
  - An `api`- or `store`-only consumer depends with `default-features = false` and links no tokio or UDS code.
