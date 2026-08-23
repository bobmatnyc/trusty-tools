Security

- Control-plane session routes (`/api/v1/control/sessions/*`) now reject non-loopback callers with 403, mirroring the `/rpc` guard, and `ctl_run_session` validates the caller-supplied `claude_cmd` (must name the `claude` binary, no shell metacharacters) and `workdir` (absolute, no `..`, existing directory) before spawning — closing a local arbitrary-execution hole where any local process could make the daemon spawn an arbitrary binary (Refs #6197).
