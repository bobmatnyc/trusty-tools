Changed
- `tm secrets exec` takes the name of the token-fd variable, `TRUSTY_SECRETS_TOKEN_FD`, from `trusty_secrets::api::TOKEN_FD_ENV`, so tm and the trusty-secrets tier-3 client read one definition (Refs #9071).
