Added
- `install-service` and `install-host` sign the installed `trusty-browser` binary in place with the team-4JH68XUHC5 Developer ID and identifier `com.trusty.browser` when it does not already satisfy that requirement, verify it, and write no plist or manifest if signing or verification fails
- `--sign-identity` and `TRUSTY_BROWSER_SIGN_IDENTITY` choose the identity; otherwise the one valid team-4JH68XUHC5 Developer ID Application identity is used, and none or several is a refusal
- `--no-sign` registers the binary unsigned, with a warning that every upgrade brings back the Keychain prompt
- Both install verbs also sign and verify the `trusty-browser-mcp` beside the installed `trusty-browser`, with the same identity and identifier, and register nothing if it fails; a missing `trusty-browser-mcp` is an error unless `--no-sign` is given
