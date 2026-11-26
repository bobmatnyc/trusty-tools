Fixed
- `credentials::test_sandbox::CredentialSandbox` removes `GIT_CONFIG_COUNT` before its `GIT_CONFIG_KEY_<n>` / `GIT_CONFIG_VALUE_<n>` pairs and restores it after them. A git run by a concurrent, unserialized test no longer fails with "missing config value GIT_CONFIG_VALUE_0" while a sandbox enters or drops (#11877).
