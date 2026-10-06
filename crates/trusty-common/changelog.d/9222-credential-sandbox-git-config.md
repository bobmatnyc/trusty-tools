Fixed
- `CredentialSandbox::enter` now removes `GIT_CONFIG_COUNT`, every `GIT_CONFIG_KEY_<n>` / `GIT_CONFIG_VALUE_<n>` pair and `GIT_CONFIG_PARAMETERS` as one set, and restores them on drop. It used to remove only the keys, so git inside the sandbox failed with "missing config key GIT_CONFIG_KEY_0" in a shell that exports `GIT_CONFIG_COUNT`, such as a tm session (#9222).
