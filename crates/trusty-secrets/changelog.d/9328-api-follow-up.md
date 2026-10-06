Changed
- `ServerSettings` and `ResolvedVar` are `#[non_exhaustive]` ([#9328](https://github.com/bobmatnyc/trusty-tools/issues/9328)); build settings with the new `ServerSettings::new(socket, index_root, machine_config, idle_timeout)`. `ResolvedConfig` implements `Default` (the Keychain backend, no vault override), so other crates can build one.
- The `keychain_roundtrip` test target requires the `store` feature, so `cargo test --no-default-features --features api` compiles.
