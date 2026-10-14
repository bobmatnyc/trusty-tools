Breaking
- `embedder_client::SupervisorConfig` gains a public `recycle: RecycleConfig` field, so a struct literal that names every field no longer compiles; add `recycle: RecycleConfig::from_env()` or end the literal with `..SupervisorConfig::default()` (#9828).
