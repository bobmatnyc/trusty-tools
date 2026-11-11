Added

- `tm secrets copy` copies one value from a named source into the project scope as the single KEY given: `--from keychain --from-service S --from-account A` for any Keychain item, or `--from <backend> --from-vault V --from-key K` for a key of any trusty vault, such as the owner vault. The value never reaches tm; output names keys only (#10998).
