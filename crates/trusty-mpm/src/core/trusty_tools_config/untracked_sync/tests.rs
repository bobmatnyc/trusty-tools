//! Unit tests for [`super`] (#2196).
//!
//! Why: split from `trusty_tools_config.rs`'s own test module when the
//! `untracked_sync` section moved into its own sibling module, keeping the
//! parent config file under the 500-SLOC production cap.
//! What: YAML round-trip for [`super::UntrackedSyncConfig`] (global +
//! per-project) and the [`super::resolve_untracked_sync`] precedence tests
//! (default / global-override / project-override / project-disabled).
//! Test: this IS the test module.

use super::*;
use crate::core::trusty_tools_config::{ProjectConfig, TrustyToolsConfig};

/// Why: the `untracked_sync:` section (global and per-project) must
/// round-trip through YAML, and an absent section must not serialise —
/// matching every other optional section's backward-compat contract.
/// Test: itself.
#[test]
fn untracked_sync_config_yaml_round_trip() {
    let cfg = TrustyToolsConfig {
        untracked_sync: Some(UntrackedSyncConfig {
            patterns: Some(vec![".env".into(), ".env.local".into()]),
            enabled: Some(true),
        }),
        projects: vec![ProjectConfig {
            name: "widget".into(),
            repo_url: "https://github.com/acme/widget".into(),
            default_branch: None,
            stack_hint: None,
            tags: None,
            description: None,
            gh_user: None,
            gh_account: None,
            github: None,
            commit_name: None,
            commit_email: None,
            untracked_sync: Some(UntrackedSyncConfig {
                patterns: Some(vec![".env.widget".into()]),
                enabled: Some(false),
            }),
        }],
        ..Default::default()
    };
    let yaml = serde_yaml::to_string(&cfg).expect("serialise");
    assert!(yaml.contains("untracked_sync"), "yaml: {yaml}");
    let back: TrustyToolsConfig = serde_yaml::from_str(&yaml).expect("deserialise");
    assert_eq!(cfg, back);

    // Absent untracked_sync sections must not serialise.
    let empty = TrustyToolsConfig::default();
    let yaml_empty = serde_yaml::to_string(&empty).expect("serialise");
    assert!(!yaml_empty.contains("untracked_sync"), "yaml: {yaml_empty}");
}

/// Why: with no config at all (fresh install), resolution must fall back
/// to the built-in default — sync ENABLED with the built-in `.env*`
/// pattern set (#2196's "operator does nothing" default).
/// Test: itself.
#[test]
fn resolve_untracked_sync_defaults_when_unset() {
    let config = TrustyToolsConfig::default();
    let resolved = resolve_untracked_sync(&config, Some("trusty-tools"));
    assert!(resolved.enabled);
    assert_eq!(
        resolved.patterns,
        DEFAULT_UNTRACKED_SYNC_PATTERNS
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>()
    );
}

/// Why: a global `untracked_sync.patterns` override must win over the
/// built-in default when no per-project override exists.
/// Test: itself.
#[test]
fn resolve_untracked_sync_global_overrides_default() {
    let config = TrustyToolsConfig {
        untracked_sync: Some(UntrackedSyncConfig {
            patterns: Some(vec![".env.global-only".into()]),
            enabled: None,
        }),
        ..Default::default()
    };
    let resolved = resolve_untracked_sync(&config, None);
    assert_eq!(resolved.patterns, vec![".env.global-only".to_string()]);
    assert!(resolved.enabled, "enabled must default to true");
}

/// Why: a per-project `untracked_sync.patterns` override must win over
/// BOTH the global section and the built-in default.
/// Test: itself.
#[test]
fn resolve_untracked_sync_project_overrides_global() {
    let config = TrustyToolsConfig {
        untracked_sync: Some(UntrackedSyncConfig {
            patterns: Some(vec![".env.global".into()]),
            enabled: Some(true),
        }),
        projects: vec![ProjectConfig {
            name: "widget".into(),
            repo_url: "https://github.com/acme/widget".into(),
            default_branch: None,
            stack_hint: None,
            tags: None,
            description: None,
            gh_user: None,
            gh_account: None,
            github: None,
            commit_name: None,
            commit_email: None,
            untracked_sync: Some(UntrackedSyncConfig {
                patterns: Some(vec![".env.widget-only".into()]),
                enabled: None,
            }),
        }],
        ..Default::default()
    };
    let resolved = resolve_untracked_sync(&config, Some("widget"));
    assert_eq!(resolved.patterns, vec![".env.widget-only".to_string()]);
    // enabled falls through project(None) -> global(Some(true)).
    assert!(resolved.enabled);

    // A DIFFERENT repo name (no matching project) must fall back to global.
    let resolved_other = resolve_untracked_sync(&config, Some("other-repo"));
    assert_eq!(resolved_other.patterns, vec![".env.global".to_string()]);
}

/// Why: a per-project `enabled: false` must disable sync for that
/// project even when the global section (or default) is enabled.
/// Test: itself.
#[test]
fn resolve_untracked_sync_disabled_by_project() {
    let config = TrustyToolsConfig {
        projects: vec![ProjectConfig {
            name: "widget".into(),
            repo_url: "https://github.com/acme/widget".into(),
            default_branch: None,
            stack_hint: None,
            tags: None,
            description: None,
            gh_user: None,
            gh_account: None,
            github: None,
            commit_name: None,
            commit_email: None,
            untracked_sync: Some(UntrackedSyncConfig {
                patterns: None,
                enabled: Some(false),
            }),
        }],
        ..Default::default()
    };
    let resolved = resolve_untracked_sync(&config, Some("widget"));
    assert!(!resolved.enabled);
    // patterns still resolve to the default even though sync is disabled
    // (the caller is expected to check `enabled` before syncing).
    assert_eq!(
        resolved.patterns,
        DEFAULT_UNTRACKED_SYNC_PATTERNS
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>()
    );
}
