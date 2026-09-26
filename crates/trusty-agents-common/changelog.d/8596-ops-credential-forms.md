Security

- The `local-ops` agent checks that a Keychain item exists by exit status alone, never adds `-w`/`-g` to a check, and consumes a needed value inside the command that uses it (a pipe to `--password-stdin`) rather than in a shell variable, matching the `tm-secrets` skill; the `gcp-ops` agent never runs `print-access-token` as its own command and consumes a token inside the command that needs it ([#8596](https://github.com/bobmatnyc/trusty-tools/issues/8596), [#8248](https://github.com/bobmatnyc/trusty-tools/issues/8248)).
