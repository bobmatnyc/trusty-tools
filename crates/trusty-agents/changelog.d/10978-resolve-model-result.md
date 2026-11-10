Changed
- `agents::resolve_model` returns `Result<(String, ModelSource), ProviderLockError>`; it is an `Err` only when a provider lock is in effect and no model is configured. `AgentConfig::try_ctrl_default` is the fallible form of `ctrl_default`, which panics under a lock (#10978).
