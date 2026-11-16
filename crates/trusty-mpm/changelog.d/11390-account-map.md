Added
- A dispatch of an ops agent (`role: ops`) now carries a `## Project accounts` block listing each service's account name, scope and read-only verify command from `[accounts.<service>]` in `.trusty-mpm.toml`, plus the rule to name the account and run its verify command before any permission request (#11390).
- `.trusty-mpm.toml` accepts `[accounts.<service>]` with `account`, `scope` and `verify`; a credential-shaped, multi-line or unknown value rejects the file at load (#11390).
