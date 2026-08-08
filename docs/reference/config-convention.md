# The `~/.trusty-tools/<crate>/config.yaml` convention

This is the cross-crate configuration standard for the trusty-tools workspace.
It gives every trusty-* crate **one** canonical place to read and write its
user-facing configuration.

## Location & format

```
~/.trusty-tools/<crate>/config.yaml
```

- `<crate>` is the crate's package name, e.g. `trusty-mpm`, `trusty-search`.
- The format is **YAML**.
- An absent file means "no config" → the crate uses its built-in defaults.
- A malformed file is logged at `warn` (to **stderr**, never stdout) and the
  crate falls back to defaults — a bad config never aborts startup.

Examples:

- `~/.trusty-tools/trusty-mpm/config.yaml`
- `~/.trusty-tools/trusty-search/config.yaml` (when that crate adopts it)

## trusty-mpm's settings (the first adopter)

`~/.trusty-tools/trusty-mpm/config.yaml`:

```yaml
# Managed-session workspace root template. A leading `~` is expanded.
# Sessions nest under <root>/<owner>/<repo>/<session-id>/.
workspace_root_template: ~/trusty-mpm-projects
# Default supervisor auto-resume preference.
auto_resume: false
# Default model id or tier alias for launched sessions.
default_model: sonnet
```

Resolution precedence for the workspace root is:

1. `TRUSTY_MPM_WORKSPACE_ROOT` environment variable (back-compat escape hatch);
2. `workspace_root_template` from this file;
3. the built-in default `~/trusty-mpm-projects`.

These settings are editable from the **trusty-console Config tab**
(`/api/console/config/mpm`, backed by the `config_read` / `config_write` MCP
tools), so operators never hand-edit the YAML unless they want to.

## Workspace-root migration

The managed-session workspace root moved from a legacy
`~/.trusty-mpm/workspaces/<project>/<session-id>/` layout to the current
`~/trusty-mpm-projects/<owner>/<repo>/<session-id>/` layout. This was a
**migration, not a hard cutover**: `trusty_mpm::core::workspace_scan` discovers
sessions under **both** roots, so pre-migration workspaces are never silently
orphaned. The authoritative session state remains
`~/.trusty-mpm/session-manager/sessions.json`; the dual-root scan is the
migration-aware filesystem discovery layer.

## Relationship to the legacy `~/.trusty-mpm/config.toml`

trusty-mpm's older `~/.trusty-mpm/config.toml` (agent sources, per-agent model
overrides, PM toggles) is **unchanged** and still read. The YAML convention
above is **additive** — it carries its own settings without disturbing the
existing TOML.

## Adding this convention to a crate

Crate maintainers implementing this convention in a new trusty-* crate should
see
[config-convention-internal.md](config-convention-internal.md) for the
shared `trusty-common` helper and the `serde_yaml` dependency notes.
