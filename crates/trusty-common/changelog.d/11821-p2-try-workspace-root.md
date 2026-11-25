Added
- `workspace_layout::try_resolve_workspace_root` returns an `InstallPathsError` when `~/.trusty-tools/install.toml` is broken and neither `TRUSTY_MPM_WORKSPACE_ROOT` nor the template decides the root. Callers that clone, delete or write under the root use it; `resolve_workspace_root` keeps its logged default for readers (#11821).
