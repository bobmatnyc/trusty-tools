Security
- `secrets.copy` carries a key's agent deny into the destination before the value; a denied key copied into a backend that cannot hold a deny (1Password, Keeper) fails with `unsupported` and nothing is written (#10386).
- `delete` removes a key's deny only after every backend's value delete succeeded, so a failed delete keeps the deny (#10386).
- A new key's `set` no longer clears a deny left from an earlier key of the same name; the deny stays (#10386).
- Every deny the store writes or removes on its own (the `KeepClosed` migration, `delete`, `copy`) has a write-ahead `secrets.set_agents_may_use` audit record with `agents_allowed`; a record that cannot be written leaves the deny unchanged (`audit_unavailable`). `SecretStore::with_deny_audit`, `store::DenyAudit` and `SecretsError::AuditUnavailable` carry it (#10386).
