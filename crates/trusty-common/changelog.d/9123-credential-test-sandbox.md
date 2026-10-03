Added

- `credentials::test_sandbox` behind the new `credential-test-sandbox` feature
  (dev-dependencies only): `CredentialSandbox::enter()` clears every
  credential-shaped environment variable, points `HOME`, the XDG dirs,
  `GH_CONFIG_DIR` and `TRUSTY_DATA_DIR_OVERRIDE` at a temp tree, latches the
  `.env.local` loader, and panics when isolation does not take.
  `assert_secret_eq` prints only a redacted preview on mismatch (#9123).
- `credentials::skip_env_local_load()` marks the one-time `.env.local` load
  done without reading a file, so a test sandbox can keep the loader off the
  developer's real `.env.local` (#9123).
