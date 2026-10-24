Breaking
- `control::config::ControlPlaneConfig` gains a public `hook_exe: Option<PathBuf>` field, the binary written into a control-plane worker's project hooks (#9704). Code that builds a `ControlPlaneConfig` with a struct literal must set it (`None` resolves the installed binary, the production behaviour). The field is `#[serde(skip)]`, so config files are unchanged.
