Breaking
- `trusty_secrets::server::backends_for` takes a third parameter, `sandbox: &SandboxEnv`, so the factory and the server agree on whose machine config enables a backend (#9793). Pass the same `SandboxEnv` the binary reads at start and gives `StartEnv`.
