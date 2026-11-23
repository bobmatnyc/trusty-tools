Fixed

- `system_status`'s credential listing no longer reads the developer's own
  `.env.local` when a caller supplies its own source. `list_status_from` takes
  a `&dyn EnvLocalSource` alongside the existing `&dyn KeyStore`, so
  `credential_status_never_leaks_a_value` reports the store tier regardless of
  what an ancestor `.env.local` binds (#5678). Tier precedence is unchanged.
