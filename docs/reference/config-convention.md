# The `~/.trusty-tools/<crate>/config.yaml` convention (#1220)

This is the cross-crate configuration standard for the trusty-tools workspace,
introduced in issue #1220 (P4 of the session-manager arc). It gives every
trusty-* crate **one** canonical place to read and write its user-facing
configuration.

## Location & format

```
~/.trusty-tools/<crate>/config.yaml
```

- `<crate>` is the crate's package name, e.g. `trusty-mpm`, `trusty-search`.
- The format is **YAML** (RFC #1225 Q4 decision).
- An absent file means "no config" → the crate uses its built-in defaults.
- A malformed file is logged at `warn` (to **stderr**, never stdout) and the
  crate falls back to defaults — a bad config never aborts startup.

Examples:

- `~/.trusty-tools/trusty-mpm/config.yaml`
- `~/.trusty-tools/trusty-search/config.yaml` (when that crate adopts it)

## YAML dependency

The convention is parsed by `serde_yaml` **0.9.x** (pure-Rust `libyaml-safer`
backend), pinned in the workspace `[workspace.dependencies]` table. The 0.8 line
(which pulls the unmaintained `unsafe-libyaml` C binding) is explicitly excluded.

`serde_yaml` 0.9 is itself in upstream maintenance mode; the full migration to a
maintained successor (`serde_yml` / `serde-yaml-ng`) across **all** crates is
tracked in **#1250** and is out of scope for #1220. Do not relax the `0.9` floor
or add a new YAML crate without coordinating with #1250.

## Adopting the convention in a crate (the shared helper)

`trusty-common` provides the convention behind its `crate-config` feature:

```toml
# Cargo.toml
trusty-common = { workspace = true, features = ["crate-config"] }
```

```rust
use serde::{Deserialize, Serialize};
use trusty_common::crate_config;

#[derive(Debug, Default, Serialize, Deserialize)]
struct MyConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    some_setting: Option<String>,
}

// Read (absent / malformed → Default), never panics:
let cfg: MyConfig = crate_config::load_or_default("trusty-search");

// Write (atomic, creates parent dirs):
crate_config::save("trusty-search", &cfg)?;

// Resolve the path (e.g. for an "edit this file" hint):
let path = crate_config::crate_config_path("trusty-search"); // Option<PathBuf>
```

Path-taking variants (`*_at`) take an explicit base directory and are used by
tests to stay hermetic.

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

## Workspace-root migration (#1220 dual-root scan)

The managed-session workspace root moved from the legacy
`~/.trusty-mpm/workspaces/<project>/<session-id>/` layout to the new
`~/trusty-mpm-projects/<owner>/<repo>/<session-id>/` layout. This is a
**migration, not a hard cutover**: `trusty_mpm::core::workspace_scan` discovers
sessions under **both** roots during the transition, so pre-#1220 workspaces are
never silently orphaned. The authoritative session state remains
`~/.trusty-mpm/session-manager/sessions.json`; the dual-root scan is the
migration-aware filesystem discovery layer.

## Relationship to the legacy `~/.trusty-mpm/config.toml`

trusty-mpm's older `~/.trusty-mpm/config.toml` (agent sources, per-agent model
overrides, PM toggles) is **unchanged** and still read. The new YAML convention
is **additive** — it carries the #1220 settings without disturbing the existing
TOML.
