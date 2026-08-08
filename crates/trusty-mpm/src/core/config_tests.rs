//! Unit tests for [`super::config`] — `~/.trusty-mpm/config.toml` parsing.
//!
//! Why: split out of `config.rs` (issue #5034) so the production file stays
//! under the 500-SLOC cap as sections are added, mirroring the
//! `project_hooks.rs` / `project_hooks_tests.rs` split already used in this
//! crate. No test was changed by the move.
//! What: covers absent/valid/malformed loading, every config section's
//! defaults, and the four-level model-resolution precedence.
//! Test: this module IS the test suite for `super`.

use super::*;

/// Helper: write `content` to `<dir>/config.toml` and load from `dir`.
fn load_from_str(dir: &Path, content: &str) -> MpmConfig {
    std::fs::write(dir.join("config.toml"), content).unwrap();
    MpmConfig::load(dir)
}

#[test]
fn config_absent_yields_defaults() {
    // An absent config.toml must silently yield the default struct.
    let dir = tempfile::TempDir::new().unwrap();
    let cfg = MpmConfig::load(dir.path());
    assert_eq!(cfg, MpmConfig::default());
    assert!(cfg.agents.sources.is_empty());
    assert!(cfg.models.agents.is_empty());
    // SM-1 zero-regression: absent [session_manager] → disabled by default.
    assert!(!cfg.session_manager.enabled);
    // #5034 zero-regression: absent [hooks] → prompt-context hook ON.
    assert!(cfg.hooks.prompt_context);
}

/// Why (#5034): the opt-out must be opt-OUT. A present-but-partial
/// `[hooks]` section, and a config that never mentions hooks at all, must
/// both leave the injection enabled — otherwise adding the section for some
/// future key would silently switch memory injection off.
#[test]
fn config_hooks_defaults_to_enabled() {
    let dir = tempfile::TempDir::new().unwrap();
    // Present-but-empty section.
    assert!(load_from_str(dir.path(), "[hooks]\n").hooks.prompt_context);
    // Unrelated section only.
    assert!(
        load_from_str(dir.path(), "[pm]\ncircuit_breaker = true\n")
            .hooks
            .prompt_context
    );
    assert!(HooksConfig::default().prompt_context);
}

/// Why (#5034): the key is the only supported way to suppress the
/// per-prompt `trusty-memory prompt-context` injection — there is
/// deliberately no CLI flag and no env var — so its parsing is load-bearing.
/// What: sets `[hooks] prompt_context = false` alongside another section and
/// asserts it parses through and leaves everything else at its default.
#[test]
fn config_hooks_prompt_context_off() {
    let dir = tempfile::TempDir::new().unwrap();
    let cfg = load_from_str(
        dir.path(),
        r#"
[models.tiers]
opus = "claude-opus-5"

[hooks]
prompt_context = false
"#,
    );
    assert!(!cfg.hooks.prompt_context);
    assert_eq!(cfg.models.tiers.opus.as_deref(), Some("claude-opus-5"));
    // Every other section stays default — the toggle is narrow.
    assert_eq!(cfg.pm, PmConfig::default());
    assert_eq!(cfg.catchup, CatchupConfig::default());
}

#[test]
fn config_valid_parsed() {
    let dir = tempfile::TempDir::new().unwrap();
    let toml = r#"
[agents]
sources = ["bundled", "user"]

[models]
default = "sonnet"

[models.agents]
engineer = "haiku"
rust-engineer = "opus"

[models.tiers]
haiku = "claude-haiku-4-5"
sonnet = "claude-sonnet-4-5"
opus = "claude-opus-4-5"

[skills]
sources = ["bundled"]

[pm]
circuit_breaker = true

[session_manager]
enabled = true

[session_manager.inference]
provider = "anthropic"
sm_model = "anthropic/claude-sonnet-4-6"
temperature = 0.4

[session_manager.memory]
palace = "session-manager"
recall_top_k = 8

[session_manager.rounds]
window = 12
"#;
    let cfg = load_from_str(dir.path(), toml);
    assert_eq!(cfg.agents.sources, vec!["bundled", "user"]);
    assert_eq!(
        cfg.models.agents.get("engineer").map(|s| s.as_str()),
        Some("haiku")
    );
    assert_eq!(
        cfg.models.agents.get("rust-engineer").map(|s| s.as_str()),
        Some("opus")
    );
    assert_eq!(cfg.models.tiers.haiku.as_deref(), Some("claude-haiku-4-5"));
    assert_eq!(cfg.models.default.as_deref(), Some("sonnet"));
    assert_eq!(cfg.skills.sources, vec!["bundled"]);
    assert_eq!(cfg.pm.circuit_breaker, Some(true));
    // [session_manager] parses through MpmConfig (DOC-14 §10).
    assert!(cfg.session_manager.enabled);
    assert_eq!(cfg.session_manager.inference.provider, "anthropic");
    assert_eq!(cfg.session_manager.inference.temperature, 0.4);
    assert_eq!(cfg.session_manager.memory.recall_top_k, 8);
    assert_eq!(cfg.session_manager.rounds.window, 12);
    // A field omitted from the partial inference block keeps its spec default.
    assert_eq!(cfg.session_manager.inference.context_token_budget, 24_000);
}

#[test]
fn config_session_manager_partial_takes_defaults() {
    // A [session_manager] section that sets only `enabled` must leave every
    // nested subsection at its spec §10 default.
    let dir = tempfile::TempDir::new().unwrap();
    let toml = r#"
[session_manager]
enabled = true
"#;
    let cfg = load_from_str(dir.path(), toml);
    assert!(cfg.session_manager.enabled);
    assert_eq!(cfg.session_manager.inference.provider, "auto");
    assert_eq!(cfg.session_manager.memory.palace, "session-manager");
    assert_eq!(cfg.session_manager.rounds.window, 10);
}

#[test]
fn config_session_manager_absent_is_noop_default() {
    // No [session_manager] section at all → the whole struct defaults and
    // the SM is disabled (zero-regression proof for SM-1).
    let dir = tempfile::TempDir::new().unwrap();
    let toml = r#"
[models.agents]
engineer = "haiku"
"#;
    let cfg = load_from_str(dir.path(), toml);
    assert_eq!(
        cfg.session_manager,
        crate::core::sm::config::SessionManagerConfig::default()
    );
    assert!(!cfg.session_manager.enabled);
}

#[test]
fn config_manifest_section_parses() {
    // HR-2: the [manifest] section must parse the catalog source overrides,
    // and an absent section must leave them all None (no behavior change).
    let dir = tempfile::TempDir::new().unwrap();
    let toml = r#"
[manifest]
repo = "https://github.com/me/my-fork"
git_ref = "dev"
ttl_hours = 6
"#;
    let cfg = load_from_str(dir.path(), toml);
    assert_eq!(
        cfg.manifest.repo.as_deref(),
        Some("https://github.com/me/my-fork")
    );
    assert_eq!(cfg.manifest.git_ref.as_deref(), Some("dev"));
    assert_eq!(cfg.manifest.ttl_hours, Some(6));

    // Absent section → all None.
    let empty = MpmConfig::load(tempfile::TempDir::new().unwrap().path());
    assert_eq!(empty.manifest, ManifestConfig::default());
}

#[test]
fn config_malformed_falls_back() {
    // A malformed config.toml must log (tested by absence of panic) and
    // return defaults — the daemon must not crash on a bad file.
    let dir = tempfile::TempDir::new().unwrap();
    let cfg = load_from_str(dir.path(), "this is not toml {{{{");
    assert_eq!(cfg, MpmConfig::default());
}

#[test]
fn config_partial_sections_are_fine() {
    // Users should be able to configure only the sections they care about.
    let dir = tempfile::TempDir::new().unwrap();
    let toml = r#"
[models.agents]
engineer = "haiku"
"#;
    let cfg = load_from_str(dir.path(), toml);
    assert_eq!(
        cfg.models.agents.get("engineer").map(|s| s.as_str()),
        Some("haiku")
    );
    // Other sections must yield defaults.
    assert!(cfg.agents.sources.is_empty());
    assert!(cfg.pm.circuit_breaker.is_none());
}

#[test]
fn tier_alias_expansion() {
    let dir = tempfile::TempDir::new().unwrap();
    let toml = r#"
[models.tiers]
haiku = "claude-haiku-4-5"
sonnet = "claude-sonnet-4-7"
opus = "claude-opus-4-7"
"#;
    let cfg = load_from_str(dir.path(), toml);
    assert_eq!(cfg.expand_model_alias("haiku"), "claude-haiku-4-5");
    assert_eq!(cfg.expand_model_alias("sonnet"), "claude-sonnet-4-7");
    assert_eq!(cfg.expand_model_alias("opus"), "claude-opus-4-7");
    // Full model ids pass through unchanged.
    assert_eq!(cfg.expand_model_alias("claude-opus-4-7"), "claude-opus-4-7");
    // "auto" maps to sonnet.
    assert_eq!(cfg.expand_model_alias("auto"), "claude-sonnet-4-5");
}

#[test]
fn tier_alias_defaults_when_not_configured() {
    // Without explicit tier config, built-in defaults must apply.
    let cfg = MpmConfig::default();
    assert_eq!(cfg.expand_model_alias("haiku"), "claude-haiku-4-5");
    assert_eq!(cfg.expand_model_alias("sonnet"), "claude-sonnet-4-5");
    assert_eq!(cfg.expand_model_alias("opus"), "claude-opus-4-5");
}

#[test]
fn config_catchup_defaults_parse() {
    // An absent [catchup] section must silently yield the default struct.
    let dir = tempfile::TempDir::new().unwrap();
    let cfg = MpmConfig::load(dir.path());
    assert!(cfg.catchup.auto, "auto defaults to true");
    assert!(cfg.catchup.include_git, "include_git defaults to true");
    assert!(
        cfg.catchup.include_palace,
        "include_palace defaults to true"
    );
    assert_eq!(cfg.catchup.git_limit, 50);
    assert_eq!(cfg.catchup.drawer_limit, 15);
}

#[test]
fn config_catchup_section_parses() {
    // A full [catchup] section must override all defaults.
    let dir = tempfile::TempDir::new().unwrap();
    let toml = r#"
[catchup]
auto = false
include_git = false
include_palace = true
git_limit = 25
drawer_limit = 5
"#;
    let cfg = load_from_str(dir.path(), toml);
    assert!(!cfg.catchup.auto);
    assert!(!cfg.catchup.include_git);
    assert!(cfg.catchup.include_palace);
    assert_eq!(cfg.catchup.git_limit, 25);
    assert_eq!(cfg.catchup.drawer_limit, 5);
}

#[test]
fn config_idle_auto_stop_defaults() {
    // An absent [idle_auto_stop] section must yield disabled (false) with
    // sensible numeric defaults — the zero-change guarantee for #1816.
    let dir = tempfile::TempDir::new().unwrap();
    let cfg = MpmConfig::load(dir.path());
    assert!(
        !cfg.idle_auto_stop.enabled,
        "idle_auto_stop must default to disabled"
    );
    assert!(
        cfg.idle_auto_stop.dry_run,
        "idle_auto_stop must default to report-only (dry_run = true) — #1783"
    );
    assert_eq!(cfg.idle_auto_stop.poll_interval_secs, 300);
    assert_eq!(cfg.idle_auto_stop.idle_consecutive_threshold, 3);
    assert_eq!(cfg.idle_auto_stop.done_consecutive_threshold, 1);
}

#[test]
fn config_idle_auto_stop_section_parses() {
    // A full [idle_auto_stop] section must override all defaults.
    let dir = tempfile::TempDir::new().unwrap();
    let toml = r#"
[idle_auto_stop]
enabled = true
dry_run = false
poll_interval_secs = 120
idle_consecutive_threshold = 5
done_consecutive_threshold = 2
"#;
    let cfg = load_from_str(dir.path(), toml);
    assert!(cfg.idle_auto_stop.enabled);
    assert!(
        !cfg.idle_auto_stop.dry_run,
        "explicit dry_run = false must disable report-only mode"
    );
    assert_eq!(cfg.idle_auto_stop.poll_interval_secs, 120);
    assert_eq!(cfg.idle_auto_stop.idle_consecutive_threshold, 5);
    assert_eq!(cfg.idle_auto_stop.done_consecutive_threshold, 2);
}

#[test]
fn config_idle_auto_stop_enabled_only_keeps_defaults() {
    // Setting only `enabled = true` must leave numeric fields at defaults
    // AND keep dry_run at its report-only default (the teardown-safe gate).
    let dir = tempfile::TempDir::new().unwrap();
    let toml = r#"
[idle_auto_stop]
enabled = true
"#;
    let cfg = load_from_str(dir.path(), toml);
    assert!(cfg.idle_auto_stop.enabled);
    assert!(
        cfg.idle_auto_stop.dry_run,
        "enabling the loop alone must NOT enact teardown — dry_run stays true (#1783)"
    );
    assert_eq!(cfg.idle_auto_stop.poll_interval_secs, 300);
    assert_eq!(cfg.idle_auto_stop.idle_consecutive_threshold, 3);
    assert_eq!(cfg.idle_auto_stop.done_consecutive_threshold, 1);
}

#[test]
fn config_idle_nudge_section_parses() {
    // Absent → disabled defaults (#2621, zero behavior change); a full
    // section overrides every field; a partial section (only `enabled`)
    // keeps the caps and message at their conservative defaults.
    let dir = tempfile::TempDir::new().unwrap();
    let absent = load_from_str(dir.path(), "");
    assert!(!absent.idle_nudge.enabled, "must default to disabled");
    assert_eq!(absent.idle_nudge.max_nudges_per_session, 2);
    assert_eq!(absent.idle_nudge.cooldown_secs, 300);
    assert_eq!(
        absent.idle_nudge.message,
        crate::core::idle_nudge::DEFAULT_NUDGE_MESSAGE
    );
    let full = load_from_str(
        dir.path(),
        "[idle_nudge]\nenabled = true\nmax_nudges_per_session = 1\ncooldown_secs = 60\nmessage = \"resume now\"\n",
    );
    assert!(full.idle_nudge.enabled);
    assert_eq!(full.idle_nudge.max_nudges_per_session, 1);
    assert_eq!(full.idle_nudge.cooldown_secs, 60);
    assert_eq!(full.idle_nudge.message, "resume now");

    let partial = load_from_str(dir.path(), "[idle_nudge]\nenabled = true\n");
    assert!(partial.idle_nudge.enabled);
    assert_eq!(partial.idle_nudge.max_nudges_per_session, 2);
    assert_eq!(partial.idle_nudge.cooldown_secs, 300);
    assert_eq!(
        partial.idle_nudge.message,
        crate::core::idle_nudge::DEFAULT_NUDGE_MESSAGE
    );
}

#[test]
fn model_resolution_precedence() {
    let dir = tempfile::TempDir::new().unwrap();
    let toml = r#"
[models]
default = "sonnet"

[models.agents]
engineer = "haiku"
"#;
    let cfg = load_from_str(dir.path(), toml);

    // 1. Explicit override wins over everything.
    let m = resolve_agent_model(&cfg, "engineer", Some("opus"), Some("claude-opus-4-5"));
    assert_eq!(m, "claude-opus-4-5");

    // 2. Config per-agent override wins over frontmatter.
    let m = resolve_agent_model(&cfg, "engineer", Some("opus"), None);
    // "engineer" maps to "haiku" → default haiku id.
    assert_eq!(m, "claude-haiku-4-5");

    // 3. Frontmatter hint wins over config default.
    let m = resolve_agent_model(&cfg, "unknown-agent", Some("opus"), None);
    assert_eq!(m, "claude-opus-4-5");

    // 4. Config default is the final fallback for unknown agents.
    let m = resolve_agent_model(&cfg, "unknown-agent", None, None);
    // "sonnet" tier default expands.
    assert_eq!(m, "claude-sonnet-4-5");

    // 5. Built-in fallback when neither config default nor anything else matches.
    let cfg_empty = MpmConfig::default();
    let m = resolve_agent_model(&cfg_empty, "nobody", None, None);
    assert_eq!(m, "claude-sonnet-4-5");
}

// ── default-model layering (#5207) ───────────────────────────────────────────
//
// Why: `TrustyToolsConfig::default_model` was written by the console Config tab
// and the `config_write` MCP tool and read by nothing. These pin the chain that
// now consumes it, and the project layer that outranks it.

/// The project's committed `.trusty-mpm.toml` tops the default-model chain.
/// Test: itself.
#[test]
fn project_default_model_tops_the_chain() {
    let cfg = MpmConfig {
        models: ModelsConfig {
            default: Some("haiku".into()),
            ..ModelsConfig::default()
        },
        ..MpmConfig::default()
    }
    .with_default_model_layers(Some("opus"), Some("sonnet"));

    assert_eq!(
        resolve_agent_model(&cfg, "nobody", None, None),
        "claude-opus-4-5",
        "the project layer must beat both host layers"
    );
}

/// The host YAML `default_model` beats the host TOML `[models] default`.
///
/// Why: that is the contract the field's own editor advertises — the console
/// placeholder reads "(unset — uses ~/.trusty-mpm/config.toml)", i.e. the TOML
/// is the FALLBACK for this field, so a set YAML value must win.
/// Test: itself.
#[test]
fn yaml_default_model_beats_toml_default() {
    let cfg = MpmConfig {
        models: ModelsConfig {
            default: Some("haiku".into()),
            ..ModelsConfig::default()
        },
        ..MpmConfig::default()
    }
    .with_default_model_layers(None, Some("opus"));

    assert_eq!(
        resolve_agent_model(&cfg, "nobody", None, None),
        "claude-opus-4-5"
    );
}

/// With neither new layer set, the TOML default is untouched — no regression.
/// Test: itself.
#[test]
fn default_model_layers_are_a_no_op_when_unset() {
    let base = MpmConfig {
        models: ModelsConfig {
            default: Some("haiku".into()),
            ..ModelsConfig::default()
        },
        ..MpmConfig::default()
    };
    let layered = base.clone().with_default_model_layers(None, None);
    assert_eq!(base, layered, "absent layers must change nothing");
}

/// A default — from any layer — still loses to the MORE SPECIFIC settings.
///
/// Why: `default_model` is a default, not an override. Wiring it must not let a
/// project silently outrank an operator's explicit `--model` or a per-agent
/// pin.
/// Test: itself.
#[test]
fn project_default_model_still_loses_to_more_specific_settings() {
    let mut models = ModelsConfig::default();
    models.agents.insert("engineer".into(), "haiku".into());
    let cfg = MpmConfig {
        models,
        ..MpmConfig::default()
    }
    .with_default_model_layers(Some("opus"), None);

    assert_eq!(
        resolve_agent_model(&cfg, "engineer", None, Some("sonnet")),
        "claude-sonnet-4-5",
        "an explicit --model must beat the project default"
    );
    assert_eq!(
        resolve_agent_model(&cfg, "engineer", None, None),
        "claude-haiku-4-5",
        "a per-agent pin must beat the project default"
    );
}
