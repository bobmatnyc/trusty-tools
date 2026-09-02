Fixed

- `blocking_loopback_client_ignores_exported_http_proxy` no longer builds its
  reqwest clients while another test is inside `setenv`/`unsetenv`. This binary
  serializes environment mutation in three domains that do not exclude each
  other — `#[serial]`'s default group, the named `dotenv_credential_env` group,
  and `data_dir::ENV_LOCK` — and reqwest re-reads `HTTP_PROXY` / `http_proxy` /
  `ALL_PROXY` at every client build, so holding one domain left the other two
  free to republish the environment mid-test. Both proxy tests now hold the
  named group and take `ENV_LOCK` (#6575).
