Added
- `tm secrets unlock` asks for Touch ID (or the login password) once, then opens the trusty-secrets 8 h unlock grant so secret reads raise no Keychain or 1Password prompt; `tm secrets lock` ends it (#10524).
