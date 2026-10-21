Breaking
- `core::session_launch::verify_sandbox_settings` takes `&SandboxInputs` in place of the workspace `&Path`, because verify needs the home and the role's `allowWrite` roots to judge an `Edit(...)` allow rule. Migration: pass the same `SandboxInputs` given to `write_sandbox_settings` (#10107).
