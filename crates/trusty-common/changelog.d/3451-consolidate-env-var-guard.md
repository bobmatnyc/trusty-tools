Changed
- Consolidated the three independent `EnvVarGuard` test helpers
  (`credentials::resolver`, `memory_core::dream`, `memory_core::semantic_consolidation`)
  into one shared `credentials::env_guard::EnvVarGuard`. All three restored a
  variable's prior value (or absence) identically on drop and relied on the
  same external `#[serial(dotenv_credential_env)]` group; the only difference
  was `semantic_consolidation`'s method being named `clear` instead of
  `remove`, now unified. One implementation around the process-env mutation
  race tracked in #3451, instead of three that could drift apart.
