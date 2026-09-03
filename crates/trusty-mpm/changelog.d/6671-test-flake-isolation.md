Fixed
- Test-only: five integration targets (`manager_routes`, `project_registry_routes`, `manager_cli_client`, `manager_inference`, `mcp_spawn_gate`) now redirect `$HOME` to a per-process scratch directory, so the project registry no longer seeds itself from the developer's real `~/.trusty-tools/trusty-mpm/config.yaml` (#6671).
- Test-only: three load-sensitive tests no longer assume a fixed timing budget — the orphan-GC canary owns its own `$HOME` instead of racing sibling `$HOME` overrides, and the statusline remote-slug and transcript-tail guards poll for their condition (#6663).
