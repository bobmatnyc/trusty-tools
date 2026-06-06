//! Workspace help.yaml validation (issue #216).
//!
//! Why: every standalone trusty-* binary ships a `help.yaml` under its crate
//! root, parsed at startup via `include_str!`. If any of those files drifts
//! into a state that no longer satisfies the [`trusty_common::help::HelpConfig`]
//! schema, the binary will panic on first run. This integration test loads
//! every help.yaml in the workspace and asserts the parse + render path
//! works, catching the regression at `cargo test -p trusty-common` time
//! rather than at the user's terminal.
//!
//! What: walks the six known help.yaml paths (relative to the workspace
//! root), loads each, and asserts every command has a non-empty description.
//! The walk uses relative paths so the test works in both the main checkout
//! and any worktree without needing a CARGO_MANIFEST_DIR resolver beyond the
//! workspace root.
//!
//! Test: `cargo test -p trusty-common --features cli-help --test help_yaml_workspace`.

#![cfg(feature = "cli-help")]

use std::path::PathBuf;

/// The six standalone trusty-* binaries that bundle a `help.yaml`.
///
/// Why: keeps the test data discoverable; if a new binary is added, this
/// list (and the per-crate wiring) must grow together.
/// What: list of (relative_path, binary_name) pairs. The binary name is what
/// each YAML's `name:` field must equal.
/// Test: covered by `every_help_yaml_parses`.
const HELP_YAMLS: &[(&str, &str)] = &[
    ("../trusty-search/help.yaml", "trusty-search"),
    ("../trusty-memory/help.yaml", "trusty-memory"),
    ("../trusty-analyze/help.yaml", "trusty-analyze"),
    ("../trusty-mpm/help.yaml", "trusty-mpm"),
    ("../trusty-git-analytics/help.yaml", "tga"),
    ("../trusty-agents/help.yaml", "tagent"),
];

#[test]
fn every_help_yaml_parses() {
    // CARGO_MANIFEST_DIR resolves to the trusty-common crate root at test
    // time, so `../trusty-search/help.yaml` etc. resolve to the sibling
    // crates' help files.
    let base: PathBuf = std::env::var("CARGO_MANIFEST_DIR")
        .expect("CARGO_MANIFEST_DIR must be set when cargo invokes the test binary")
        .into();
    for (rel, expected_name) in HELP_YAMLS {
        let path = base.join(rel);
        let yaml = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read help.yaml at {}: {e}", path.display()));
        let config = trusty_common::help::load_help(&yaml)
            .unwrap_or_else(|e| panic!("{} failed to parse as HelpConfig: {e}", path.display()));
        assert_eq!(
            config.name,
            *expected_name,
            "{}: name field must equal '{expected_name}', got '{}'",
            path.display(),
            config.name,
        );
        assert!(
            !config.tagline.is_empty(),
            "{}: tagline must be non-empty",
            path.display()
        );
        assert!(
            !config.usage.is_empty(),
            "{}: usage must be non-empty",
            path.display()
        );
        assert!(
            !config.commands.is_empty(),
            "{}: at least one command must be defined",
            path.display()
        );
        for (cmd_name, cmd) in &config.commands {
            assert!(
                !cmd.description.is_empty(),
                "{}: command '{cmd_name}' has empty description",
                path.display()
            );
        }
        // Render the top-level help and a randomly-picked subcommand — if
        // the format strings or padding logic regress, this catches it.
        let _ = trusty_common::help::render_help(&config, None);
        if let Some(first) = config.commands.keys().next() {
            let _ = trusty_common::help::render_help(&config, Some(first));
        }
    }
}

#[test]
fn every_help_yaml_has_working_suggester() {
    // Spot-check: take the first command name in each YAML, drop a single
    // character to simulate a typo, and assert the suggester proposes the
    // original. This guards against `suggest: false` accidentally turning
    // off the feature for any binary.
    let base: PathBuf = std::env::var("CARGO_MANIFEST_DIR")
        .expect("CARGO_MANIFEST_DIR must be set")
        .into();
    for (rel, _) in HELP_YAMLS {
        let path = base.join(rel);
        let yaml = std::fs::read_to_string(&path).unwrap();
        let config = trusty_common::help::load_help(&yaml).unwrap();
        // Pick a command name with length >= 4 so we can drop a char and
        // still leave the Jaro-Winkler similarity above threshold.
        let target = config
            .commands
            .keys()
            .find(|name| name.len() >= 4)
            .expect("every help.yaml should have at least one >=4-char command");
        // Drop the last character to form the typo.
        let typo = &target[..target.len() - 1];
        let hint = trusty_common::help::suggest(typo, &config).unwrap_or_else(|| {
            panic!(
                "{}: suggester returned None for typo '{typo}' (target '{target}')",
                path.display()
            )
        });
        assert!(
            hint.contains(target.as_str()),
            "{}: suggestion '{hint}' did not contain expected command '{target}'",
            path.display()
        );
    }
}
