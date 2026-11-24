Security
- `secrets.unlock` no longer holds a vault-blob snapshot made stale by a `set`, `delete` or agent-deny write that lands while the blob read waits on a Keychain prompt. The grant reads the blob once more, and refuses to open if the snapshot is stale again, so a key the owner just denied is never served to an agent from memory (#11786).
- The corrupt-vault-blob error now says that `tm secrets migrate --rollback` recovers only when a migration backup exists, and that it restores the backup's values (#11786).
