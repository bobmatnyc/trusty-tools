Added
- Under `TRUSTY_SANDBOX=1`, with `TRUSTY_DATA_DIR_OVERRIDE` an existing absolute directory and `$HOME` not the account's home, the server reads `file` consent from `$HOME/.trusty-tools/trusty-common/config.yaml`, so a sandbox can write `file` values (Refs #9793).
- Under `TRUSTY_SANDBOX=1` the server opens only the `file` backend: the Keychain and the CLI backends answer `backend_not_enabled` for every operation, and the delete sweep skips them (Refs #9793).
