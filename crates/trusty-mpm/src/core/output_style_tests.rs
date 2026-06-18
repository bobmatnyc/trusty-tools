//! Unit tests for [`crate::core::output_style`] (HR-4).
//!
//! Why: HR-4 acceptance requires the registry to resolve all three ids, an
//! unknown id to error, the version gate to select native-vs-inject correctly
//! against mocked version strings, and the injected prompt to contain the style
//! content for old versions but not for new ones.
//! What: covers `resolve_style`, `active_style_id`/`resolve_active_style`,
//! `parse_claude_version`, `version_supports_native`,
//! `detect_native_support_from_output`, `inject_style_into_prompt`, and
//! `maybe_inject_active_style`.
//! Test: this IS the test module.

use super::*;
use crate::core::bundle::DEFAULT_OUTPUT_STYLE_ID;
use crate::core::config::MpmConfig;

// ── Registry resolution ──────────────────────────────────────────────

#[test]
fn resolve_style_resolves_all_three_ids() {
    for id in ["trusty-mpm", "trusty-mpm-teacher", "trusty-mpm-research"] {
        let style = resolve_style(id).unwrap_or_else(|_| panic!("id {id} should resolve"));
        assert_eq!(style.id, id);
        assert!(!style.content.trim().is_empty(), "{id} content non-empty");
        // The frontmatter name must match the registry id, or Claude Code
        // silently falls back to the default style.
        assert!(
            style.content.contains(&format!("name: {id}")),
            "frontmatter name for {id} must match its registry id"
        );
    }
}

#[test]
fn unknown_style_id_errors() {
    let err = resolve_style("nope").unwrap_err();
    match err {
        StyleError::Unknown { requested, valid } => {
            assert_eq!(requested, "nope");
            // The error lists the valid ids so the message is actionable.
            assert!(valid.contains("trusty-mpm"));
            assert!(valid.contains("trusty-mpm-teacher"));
            assert!(valid.contains("trusty-mpm-research"));
        }
    }
}

#[test]
fn valid_style_ids_lists_all() {
    let ids = valid_style_ids();
    assert!(ids.contains("trusty-mpm"));
    assert!(ids.contains("trusty-mpm-teacher"));
    assert!(ids.contains("trusty-mpm-research"));
}

// ── Active-style precedence (config + override) ──────────────────────

#[test]
fn active_style_id_defaults_when_unset() {
    let cfg = MpmConfig::default();
    assert_eq!(active_style_id(&cfg, None), DEFAULT_OUTPUT_STYLE_ID);
}

#[test]
fn active_style_id_precedence() {
    let mut cfg = MpmConfig::default();
    cfg.style.active = Some("trusty-mpm-teacher".to_string());

    // Config value applies when no explicit override.
    assert_eq!(active_style_id(&cfg, None), "trusty-mpm-teacher");
    // Explicit override wins over config.
    assert_eq!(
        active_style_id(&cfg, Some("trusty-mpm-research")),
        "trusty-mpm-research"
    );
}

#[test]
fn resolve_active_style_uses_config() {
    let mut cfg = MpmConfig::default();
    cfg.style.active = Some("trusty-mpm-research".to_string());
    let style = resolve_active_style(&cfg, None).unwrap();
    assert_eq!(style.id, "trusty-mpm-research");
}

#[test]
fn resolve_active_style_unknown_errors() {
    let mut cfg = MpmConfig::default();
    cfg.style.active = Some("bogus".to_string());
    assert!(resolve_active_style(&cfg, None).is_err());
}

#[test]
fn config_style_section_parses() {
    // The `[style]` section round-trips through the real MpmConfig loader.
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(
        dir.path().join("config.toml"),
        "[style]\nactive = \"trusty-mpm-teacher\"\n",
    )
    .unwrap();
    let cfg = MpmConfig::load(dir.path());
    assert_eq!(cfg.style.active.as_deref(), Some("trusty-mpm-teacher"));
}

#[test]
fn config_absent_style_defaults_to_professional() {
    let cfg = MpmConfig::default();
    assert!(cfg.style.active.is_none());
    assert_eq!(active_style_id(&cfg, None), "trusty-mpm");
}

// ── Version parsing + gate ───────────────────────────────────────────

#[test]
fn parse_version_plain() {
    assert_eq!(parse_claude_version("1.0.83"), Some((1, 0, 83)));
}

#[test]
fn parse_version_with_suffix_text() {
    assert_eq!(
        parse_claude_version("1.0.84 (Claude Code)"),
        Some((1, 0, 84))
    );
}

#[test]
fn parse_version_with_v_prefix() {
    assert_eq!(parse_claude_version("claude v2.3.1"), Some((2, 3, 1)));
}

#[test]
fn parse_version_prerelease_patch() {
    // A pre-release suffix on the patch component is tolerated.
    assert_eq!(parse_claude_version("1.0.90-beta.2"), Some((1, 0, 90)));
}

#[test]
fn parse_version_unparseable() {
    assert_eq!(parse_claude_version("no version here"), None);
    assert_eq!(parse_claude_version(""), None);
    // Two-component strings are not a full triple.
    assert_eq!(parse_claude_version("1.0"), None);
}

#[test]
fn version_gate_supported() {
    assert!(version_supports_native((1, 0, 84)));
    assert!(version_supports_native((1, 1, 0)));
    assert!(version_supports_native((2, 0, 0)));
}

#[test]
fn version_gate_exact_floor() {
    // The floor itself supports native output styles (inclusive bound).
    assert!(version_supports_native(NATIVE_OUTPUT_STYLE_MIN_VERSION));
    assert!(version_supports_native((1, 0, 83)));
}

#[test]
fn version_gate_unsupported() {
    assert!(!version_supports_native((1, 0, 82)));
    assert!(!version_supports_native((1, 0, 0)));
    assert!(!version_supports_native((0, 9, 99)));
}

#[test]
fn detect_from_output_modern() {
    assert!(detect_native_support_from_output("1.0.84 (Claude Code)"));
    assert!(detect_native_support_from_output("1.0.83"));
}

#[test]
fn detect_from_output_old() {
    assert!(!detect_native_support_from_output("1.0.50 (Claude Code)"));
}

#[test]
fn detect_from_output_unparseable_fails_safe() {
    // Unparseable output → assume NO native support (so the style is injected).
    assert!(!detect_native_support_from_output("garbage"));
    assert!(!detect_native_support_from_output(""));
}

// ── Prompt injection ─────────────────────────────────────────────────

#[test]
fn strip_frontmatter_removes_block() {
    let style = resolve_style("trusty-mpm").unwrap();
    let body = strip_frontmatter(style.content);
    assert!(
        !body.contains("name: trusty-mpm"),
        "frontmatter name line must be stripped from the injected body"
    );
    assert!(
        body.contains("# Trusty Multi-Agent PM"),
        "the style body must survive frontmatter stripping"
    );
}

#[test]
fn strip_frontmatter_passthrough() {
    // Content without frontmatter is returned (trimmed) unchanged.
    let plain = "# Just a heading\n\nbody";
    assert_eq!(strip_frontmatter(plain), plain);
}

#[test]
fn inject_prepends_style_block() {
    let style = resolve_style("trusty-mpm-teacher").unwrap();
    let prompt = "# PM Floor\n\noriginal prompt body".to_string();
    let injected = inject_style_into_prompt(style, &prompt);

    // The injected heading and the style body appear FIRST.
    assert!(injected.starts_with(INJECTED_STYLE_HEADING));
    assert!(injected.contains("# Trusty Multi-Agent PM — Teaching Mode"));
    // The original prompt is preserved verbatim, after the style block.
    assert!(injected.contains("original prompt body"));
    let style_pos = injected.find("Teaching Mode").unwrap();
    let prompt_pos = injected.find("original prompt body").unwrap();
    assert!(style_pos < prompt_pos, "style block precedes the PM prompt");
}

#[test]
fn inject_preserves_prompt() {
    let style = resolve_style("trusty-mpm").unwrap();
    let prompt = "UNIQUE_FLOOR_MARKER".to_string();
    let injected = inject_style_into_prompt(style, &prompt);
    assert!(injected.contains("UNIQUE_FLOOR_MARKER"));
}

// ── End-to-end gate: maybe_inject_active_style ───────────────────────

#[test]
fn maybe_inject_skips_when_native_supported() {
    // Modern Claude Code → native key handles it → prompt is unchanged.
    let cfg = MpmConfig::default();
    let prompt = "PM_PROMPT_BODY".to_string();
    let out = maybe_inject_active_style(&cfg, None, prompt.clone(), true);
    assert_eq!(out, prompt, "no injection when native support is present");
    assert!(!out.contains(INJECTED_STYLE_HEADING));
}

#[test]
fn maybe_inject_injects_when_native_unsupported() {
    // Old Claude Code → inject the active (default) style into the prompt.
    let cfg = MpmConfig::default();
    let prompt = "PM_PROMPT_BODY".to_string();
    let out = maybe_inject_active_style(&cfg, None, prompt, false);
    assert!(out.contains(INJECTED_STYLE_HEADING));
    assert!(out.contains("# Trusty Multi-Agent PM"));
    assert!(out.contains("PM_PROMPT_BODY"));
}

#[test]
fn maybe_inject_uses_configured_style() {
    let mut cfg = MpmConfig::default();
    cfg.style.active = Some("trusty-mpm-research".to_string());
    let out = maybe_inject_active_style(&cfg, None, "PROMPT".to_string(), false);
    assert!(out.contains("# Trusty Multi-Agent PM — Research Mode"));
}

#[test]
fn maybe_inject_explicit_overrides_config() {
    let mut cfg = MpmConfig::default();
    cfg.style.active = Some("trusty-mpm-research".to_string());
    let out = maybe_inject_active_style(
        &cfg,
        Some("trusty-mpm-teacher"),
        "PROMPT".to_string(),
        false,
    );
    assert!(out.contains("# Trusty Multi-Agent PM — Teaching Mode"));
    assert!(!out.contains("Research Mode"));
}

#[test]
fn maybe_inject_unknown_falls_back_to_default() {
    // DOC-17: an unknown style id falls back to the professional default rather
    // than failing the launch.
    let mut cfg = MpmConfig::default();
    cfg.style.active = Some("does-not-exist".to_string());
    let out = maybe_inject_active_style(&cfg, None, "PROMPT".to_string(), false);
    assert!(out.contains(INJECTED_STYLE_HEADING));
    // Default professional style body, not teaching/research.
    assert!(out.contains("# Trusty Multi-Agent PM"));
    assert!(!out.contains("Teaching Mode"));
    assert!(!out.contains("Research Mode"));
    assert!(out.contains("PROMPT"));
}
