Fixed
- Verify-before-finish gate no longer refuses `finish_task` when nothing could
  satisfy it: it now requires a detectable test command for the bound project
  (a root `Cargo.toml`, a `package.json` with a `test` script, a pytest config,
  or `go.mod` — or a non-empty root plus a prompt-named command) AND a `bash`
  tool in the agent's registry. Otherwise the finish is accepted and the
  completion report records that no test command ran and why. A refusal now
  names the manifest or command it detected. (#8206)
