Security

- The `local-ops` agent checks that a Keychain item exists by exit status alone and never adds `-w`/`-g` to a check; the `gcp-ops` agent never runs `print-access-token` as its own command and consumes a token inside the command that needs it ([#8596](https://github.com/bobmatnyc/trusty-tools/issues/8596), [#8248](https://github.com/bobmatnyc/trusty-tools/issues/8248)).
