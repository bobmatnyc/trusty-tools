# Crate-config convention — internal notes (#1220)

Internal companion to [config-convention.md](config-convention.md) (the
public-facing `~/.trusty-tools/<crate>/config.yaml` convention). This page
covers the crate-authoring side: the shared `trusty-common` helper and the
`serde_yaml` dependency decision behind it. Introduced in issue #1220 (P4 of
the session-manager arc).

## YAML dependency

The convention is parsed by `serde_yaml` **0.9.x**, pinned in the workspace
`[workspace.dependencies]` table. `serde_yaml` 0.9 resolves to the
`unsafe-libyaml` backend (dtolnay's c2rust transliteration of libyaml, not the
pure-Rust `libyaml-safer` fork) — there are no open advisories against it
today, but the 0.8 line (which has known parsing soundness issues fixed in
0.9) is explicitly excluded.

`serde_yaml` 0.9 is itself in upstream maintenance mode. `serde_yml` — once
considered a "maintained successor" per the old #1250 direction — turned out to
be archived upstream and unsound (GHSA-hhw4-xg65-fp2x / GHSA-gfxp-f68g-8x78,
Dependabot #42/#43); trusty-agents and trusty-search were migrated back onto
`serde_yaml` in #2991. `serde_yaml` is the workspace standard going forward. Do
not relax the `0.9` floor or reintroduce `serde_yml`/`libyml`.

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
