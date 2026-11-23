Added
- `tm doctor` has a `secrets_signature` row. It compares the installed `trusty-secrets` cdhash with the ACLs of the trusty Keychain items. A re-signed binary that will raise a Keychain prompt is a WARN, a binary no ACL trusts is a FAIL, and a refused or hung probe is UNKNOWN. Each drift row names the `tm secrets unlock` re-approval step.
