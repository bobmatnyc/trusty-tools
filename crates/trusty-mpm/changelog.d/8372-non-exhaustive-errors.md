Breaking
- This 1.x release carries four library API breaks against 1.6.3, shipped under an owner-approved override of the semver gate ([#8372](https://github.com/bobmatnyc/trusty-tools/issues/8372)). The `tm` binary's behaviour is unaffected.
- `ManagedError` gained the `ResumeInFlight` and `AutoResumeRecorded` variants, and `ResumeManagedError` gained `AlreadyResuming` (#8233). An exhaustive `match` on either enum outside trusty-mpm no longer compiles.
- `runtime::build_adapter` takes a fourth parameter, `framework_root: &Path`, the framework root the Claude Code adapter writes its managed config under (#8233).
- `ClaudeCodeAdapter::new` takes a third parameter, `framework_root: &Path`, for the same reason (#8233).
- `ManagedError`, `ResumeManagedError` and `runtime::launch_spec::LaunchSpecError` are now `#[non_exhaustive]`, so a later variant is not an API break. A `match` on them outside trusty-mpm needs a wildcard arm.
