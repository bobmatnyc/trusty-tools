Added

- `credentials::test_sandbox` behind the new `credential-test-sandbox` feature
  (dev-dependencies only): `CredentialSandbox::enter()` clears every
  credential-shaped environment variable, points `HOME`, the XDG dirs,
  `GH_CONFIG_DIR` and `TRUSTY_DATA_DIR_OVERRIDE` at a temp tree, sets
  `GIT_CONFIG_NOSYSTEM=1` and `GIT_TERMINAL_PROMPT=0`, keeps the one-time
  `.env.local` loader from reading a file, and panics when isolation does not
  take. While a sandbox is live, `default_store()` skips the OS keychain and
  `env_local_value()` returns `None`; a build without the feature cannot turn
  either tier off. `assert_secret_eq` prints only a redacted preview on
  mismatch (#9123).
