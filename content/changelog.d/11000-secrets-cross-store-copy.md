Changed

- The `tm-secrets` skill (1.7.0) and the `secrets-manager` agent (0.5.0) allow a copy between stores with `tm secrets copy`, from a named Keychain item or trusty vault key (#10998), while `security -w`/`-g` stay forbidden. They say an agent's copy of a Keychain item lands push-only, that a named copy cannot name a sync target, that a key denied to agents stays blocked, and that each copied key leaves one value-free audit record. The agent drops the stale claim that the `tm secrets` CLI and `exec` do not ship (#11000).
