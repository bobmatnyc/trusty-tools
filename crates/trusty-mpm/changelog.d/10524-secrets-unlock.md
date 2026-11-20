Added
- `tm secrets unlock` asks for Touch ID (or the login password) once, then opens the trusty-secrets 8 h unlock grant so secret reads raise no Keychain or 1Password prompt; `tm secrets lock` ends it (#10524).
- `tm secrets migrate [--dry-run] [--finish | --rollback]` moves every trusty Keychain item into one Keychain item, so `tm secrets unlock` raises one prompt in total; `--rollback` restores the per-item layout (#10524).
