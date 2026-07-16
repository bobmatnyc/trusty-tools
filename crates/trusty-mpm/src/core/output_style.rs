//! Multi-style output support with version-fallback injection (HR-4).
//!
//! Why: trusty-mpm bundles three Claude Code output styles — professional
//! (`trusty-mpm`), teaching (`trusty-mpm-teacher`), and research
//! (`trusty-mpm-research`). The operator picks the active one via config
//! (`[style] active = …`) or the launch `--style` flag. Modern Claude Code
//! (≥ 1.0.83) honours a native `"outputStyle"` key in `.claude/settings.json`,
//! but older builds ignore it; for those, trusty-mpm must INJECT the active
//! style's content into the PM system prompt instead so the active style is in
//! effect regardless of Claude Code version (DOC-17 §HR-4).
//! What: a registry resolver over [`crate::core::bundle::OUTPUT_STYLES`]
//! ([`resolve_style`], [`resolve_active_style`]), a `[style]` config section
//! ([`StyleConfig`]), Claude Code version detection
//! ([`claude_supports_native_output_style`], [`version_supports_native`]), and
//! prompt injection ([`inject_style_into_prompt`]).
//! Test: `output_style_tests.rs` covers registry resolution (all three ids +
//! unknown-id error), config selection, the version gate (mocked version
//! strings), and prompt injection.

use std::path::Path;

use crate::core::bundle::{BundledStyle, DEFAULT_OUTPUT_STYLE_ID, OUTPUT_STYLES};
use crate::core::config::MpmConfig;

/// The first Claude Code version that honours the native `outputStyle` settings
/// key, as `(major, minor, patch)`.
///
/// Why: claude-mpm established `1.0.83` as the floor for native output-style
/// support; trusty-mpm mirrors it so the version gate matches upstream behaviour.
/// What: the inclusive lower bound — a detected version `>=` this supports native
/// output styles.
/// Test: `version_gate_*` in `output_style_tests.rs`.
pub const NATIVE_OUTPUT_STYLE_MIN_VERSION: (u32, u32, u32) = (1, 0, 83);

/// A failure resolving a requested output-style id.
///
/// Why: an unknown style id is a user-facing error (a typo in config or the
/// `--style` flag); callers need a typed surface that names the bad id and the
/// valid set so the message is actionable.
/// What: a single variant carrying the requested id.
/// Test: `unknown_style_id_errors` in `output_style_tests.rs`.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum StyleError {
    /// The requested style id is not in the bundled registry.
    #[error("unknown output style '{requested}'; valid styles: {valid}")]
    Unknown {
        /// The id the caller asked for.
        requested: String,
        /// Comma-separated list of valid ids (for the error message).
        valid: String,
    },
}

/// Resolve a style id to its bundled entry, erroring on an unknown id.
///
/// Why: the launch path must validate a configured/selected id against the
/// bundle before writing it into `.claude/settings.json` — Claude Code silently
/// falls back to the operator's default on a name mismatch, so a typo would
/// otherwise produce a confusing "style not applied" with no error.
/// What: linear-scans [`OUTPUT_STYLES`] for `id`; returns the matching
/// [`BundledStyle`] or [`StyleError::Unknown`] listing the valid ids.
/// Test: `resolve_style_resolves_all_three_ids`, `unknown_style_id_errors`.
pub fn resolve_style(id: &str) -> Result<&'static BundledStyle, StyleError> {
    OUTPUT_STYLES
        .iter()
        .find(|s| s.id == id)
        .ok_or_else(|| StyleError::Unknown {
            requested: id.to_string(),
            valid: valid_style_ids(),
        })
}

/// Comma-separated list of bundled style ids, in registry order.
///
/// Why: used in the [`StyleError::Unknown`] message and exposed so the CLI can
/// list the choices for `--style`.
/// What: joins every [`BundledStyle::id`] with `", "`.
/// Test: `valid_style_ids_lists_all`.
pub fn valid_style_ids() -> String {
    OUTPUT_STYLES
        .iter()
        .map(|s| s.id)
        .collect::<Vec<_>>()
        .join(", ")
}

/// `[style]` section of `~/.trusty-mpm/config.toml`.
///
/// Why: HR-4 makes the active output style configurable; this section records
/// the operator's choice so every launch picks it up without a flag.
/// What: an optional `active` id. `None` (absent section/key) means "use the
/// professional default" ([`DEFAULT_OUTPUT_STYLE_ID`]).
/// Test: `config_style_section_parses`, `active_style_id_defaults_when_unset`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
pub struct StyleConfig {
    /// The active output-style id (e.g. `"trusty-mpm-teacher"`). `None` → default.
    pub active: Option<String>,
}

/// Determine the effective active style id from config + an optional override.
///
/// Why: the active style can come from the launch `--style` flag (highest
/// precedence) or the `[style] active` config key, falling back to the
/// professional default; centralizing the precedence keeps the CLI and the
/// client launch paths consistent.
/// What: returns the `explicit` override if `Some`, else `config.style.active`
/// if set, else [`DEFAULT_OUTPUT_STYLE_ID`]. Does NOT validate the id — callers
/// validate via [`resolve_style`] (or [`resolve_active_style`]) so the
/// unknown-id error surfaces once, with full context.
/// Test: `active_style_id_precedence`.
pub fn active_style_id<'a>(config: &'a MpmConfig, explicit: Option<&'a str>) -> &'a str {
    if let Some(id) = explicit {
        return id;
    }
    config
        .style
        .active
        .as_deref()
        .unwrap_or(DEFAULT_OUTPUT_STYLE_ID)
}

/// Resolve the active style to a bundled entry, applying config + override.
///
/// Why: the launch path needs the actual [`BundledStyle`] (id + content) for the
/// active selection, validated; this combines [`active_style_id`] with
/// [`resolve_style`].
/// What: computes the active id via [`active_style_id`], then resolves it.
/// Returns [`StyleError::Unknown`] when the selected id is not bundled.
/// Test: `resolve_active_style_uses_config`, `resolve_active_style_unknown_errors`.
pub fn resolve_active_style(
    config: &MpmConfig,
    explicit: Option<&str>,
) -> Result<&'static BundledStyle, StyleError> {
    resolve_style(active_style_id(config, explicit))
}

/// Parse a `claude --version` output line into `(major, minor, patch)`.
///
/// Why: the version gate must turn the raw CLI output (e.g.
/// `"1.0.84 (Claude Code)"`) into a comparable tuple; isolating the parse makes
/// it testable without spawning the binary.
/// What: extracts the first `\d+.\d+.\d+` triple from `raw`. Returns `None` when
/// no such triple is present (so callers can treat unparseable output as "assume
/// no native support" to stay safe).
/// Test: `parse_version_*`.
pub fn parse_claude_version(raw: &str) -> Option<(u32, u32, u32)> {
    // Find the first whitespace/paren-delimited token that looks like a semver
    // triple. We scan token-by-token rather than pull in a regex dependency.
    for token in raw.split(|c: char| c.is_whitespace() || c == '(' || c == ')' || c == 'v') {
        if let Some(v) = parse_triple(token) {
            return Some(v);
        }
    }
    None
}

/// Parse a single token as a `major.minor.patch` triple.
///
/// Why: helper for [`parse_claude_version`]; a token may carry a pre-release or
/// build suffix (`1.0.84-beta`), so we split on the first non-digit after the
/// patch number.
/// What: splits `token` on `.`; requires at least three leading numeric
/// components; ignores any suffix on the patch component.
/// Test: covered via `parse_version_*`.
fn parse_triple(token: &str) -> Option<(u32, u32, u32)> {
    let mut parts = token.split('.');
    let major = parts.next()?.parse::<u32>().ok()?;
    let minor = parts.next()?.parse::<u32>().ok()?;
    // Patch may have a trailing suffix (e.g. `83-beta`); take the leading digits.
    let patch_raw = parts.next()?;
    let patch_digits: String = patch_raw
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    if patch_digits.is_empty() {
        return None;
    }
    let patch = patch_digits.parse::<u32>().ok()?;
    Some((major, minor, patch))
}

/// Decide whether a detected version supports the native `outputStyle` key.
///
/// Why: the launch path branches on this — native key vs. prompt injection.
/// What: returns `true` when `version >= ` [`NATIVE_OUTPUT_STYLE_MIN_VERSION`].
/// Test: `version_gate_supported`, `version_gate_unsupported`,
/// `version_gate_exact_floor`.
pub fn version_supports_native(version: (u32, u32, u32)) -> bool {
    version >= NATIVE_OUTPUT_STYLE_MIN_VERSION
}

/// Detect whether the installed Claude Code supports native output styles.
///
/// Why: when the installed CLI is too old (or undetectable) trusty-mpm must
/// inject the style into the PM prompt instead of relying on the settings key.
/// What: runs `claude --version`, parses the first semver triple, and compares
/// it to [`NATIVE_OUTPUT_STYLE_MIN_VERSION`]. **Fails safe**: any error (binary
/// missing, non-zero exit, unparseable output) is logged to stderr and treated
/// as "does NOT support native" so the style is always applied via injection.
/// All logging goes to stderr (the daemon/MCP stdout-clean rule).
/// Test: pure logic is in [`version_supports_native`] /
/// [`detect_native_support_from_output`]; this thin wrapper is side-effecting
/// (spawns a process) and is covered by `detect_from_output_*`.
pub fn claude_supports_native_output_style() -> bool {
    // Spawn through the disclaim helper so this direct child of the signed
    // daemon is its own TCC responsible process, not `trusty-mpm` (issue #2819).
    match crate::core::spawn_disclaim::disclaimed_output("claude", &["--version".to_string()]) {
        Ok(out) if out.status.success() => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            detect_native_support_from_output(&stdout)
        }
        Ok(out) => {
            let code = out.status.code().unwrap_or(-1);
            tracing::warn!(
                exit = code,
                "`claude --version` exited non-zero; assuming no native output-style support"
            );
            false
        }
        Err(err) => {
            tracing::warn!(
                %err,
                "could not run `claude --version`; assuming no native output-style support"
            );
            false
        }
    }
}

/// Map a `claude --version` output string to native-support, failing safe.
///
/// Why: separating the string→bool decision from the process spawn makes the
/// version gate fully unit-testable with mocked version strings (the HR-4
/// acceptance criterion).
/// What: parses `raw` via [`parse_claude_version`]; an unparseable string yields
/// `false` (assume no native support → inject). A parsed version defers to
/// [`version_supports_native`].
/// Test: `detect_from_output_modern`, `detect_from_output_old`,
/// `detect_from_output_unparseable`.
pub fn detect_native_support_from_output(raw: &str) -> bool {
    match parse_claude_version(raw) {
        Some(v) => version_supports_native(v),
        None => {
            tracing::warn!(
                output = raw.trim(),
                "could not parse Claude Code version; assuming no native output-style support"
            );
            false
        }
    }
}

/// Strip a leading YAML frontmatter block (`--- … ---`) from style content.
///
/// Why: when injecting a style into the PM system prompt, the
/// `name:`/`description:` frontmatter is Claude-Code settings metadata, not
/// instructions for the model; stripping it keeps the injected text clean.
/// What: if `content` starts with a `---` line, drops everything through the
/// closing `---` line; otherwise returns `content` unchanged (trimmed).
/// Test: `strip_frontmatter_removes_block`, `strip_frontmatter_passthrough`.
fn strip_frontmatter(content: &str) -> &str {
    let trimmed = content.trim_start();
    if let Some(rest) = trimmed.strip_prefix("---") {
        // `rest` begins right after the opening `---`. Find the closing fence.
        if let Some(end) = rest.find("\n---") {
            // Skip past the closing `---` line.
            let after = &rest[end + "\n---".len()..];
            // Drop the remainder of the closing fence line.
            if let Some(nl) = after.find('\n') {
                return after[nl + 1..].trim_start();
            }
            return after.trim_start();
        }
    }
    trimmed
}

/// Header that delimits an injected output-style block in the PM prompt.
///
/// Why: a clearly-labelled fence tells the launched PM (and a human reading the
/// stashed prompt) that the block is the active output style, injected because
/// the installed Claude Code lacks native support.
/// What: the Markdown heading prepended to the injected style body.
/// Test: `inject_prepends_style_block`.
pub const INJECTED_STYLE_HEADING: &str =
    "# Active Output Style (injected — Claude Code lacks native outputStyle support)";

/// Prepend an output style's content to a PM system prompt.
///
/// Why: on older Claude Code the `"outputStyle"` settings key is ignored, so the
/// only way to make the active style take effect is to fold its instructions
/// into the `--append-system-prompt` text. claude-mpm does the same.
/// What: returns a new string of `INJECTED_STYLE_HEADING`, then the style body
/// (frontmatter stripped), then the framework section separator, then the
/// original `prompt`. The original prompt is preserved verbatim so the PM floor
/// and overrides are untouched — the style is purely additive and placed first.
/// Test: `inject_prepends_style_block`, `inject_preserves_prompt`.
pub fn inject_style_into_prompt(style: &BundledStyle, prompt: &str) -> String {
    let body = strip_frontmatter(style.content).trim();
    format!(
        "{INJECTED_STYLE_HEADING}\n\n{body}{sep}{prompt}",
        sep = crate::core::instruction_pipeline::SECTION_SEPARATOR,
    )
}

/// Resolve the active style and inject it into `prompt` only when the installed
/// Claude Code lacks native output-style support.
///
/// Why: this is the single launch-time seam (`build_system_prompt_for` calls it)
/// that makes the active style effective regardless of Claude Code version: on a
/// modern CLI the prompt is returned unchanged (native key handles the style); on
/// an old/undetectable CLI the style is injected. Keeping it here means both the
/// CLI and client launch paths get identical behaviour.
/// What: if [`claude_supports_native_output_style`] is `true`, returns `prompt`
/// unchanged. Otherwise resolves the active style (config + `explicit`
/// override); on success injects it via [`inject_style_into_prompt`]; on an
/// unknown id logs a warning to stderr and falls back to the professional
/// default style (per DOC-17: unknown style → `trusty-mpm` default). When the
/// home dir / config is unavailable the caller passes [`MpmConfig::default`].
/// Test: `maybe_inject_*` in `output_style_tests.rs` drive the gate via the
/// injected `native_supported` flag.
pub fn maybe_inject_active_style(
    config: &MpmConfig,
    explicit: Option<&str>,
    prompt: String,
    native_supported: bool,
) -> String {
    if native_supported {
        return prompt;
    }
    let style = match resolve_active_style(config, explicit) {
        Ok(s) => s,
        Err(err) => {
            tracing::warn!(
                %err,
                "falling back to the default output style for prompt injection"
            );
            // Default is always present in the registry; resolve it directly.
            match resolve_style(DEFAULT_OUTPUT_STYLE_ID) {
                Ok(s) => s,
                Err(_) => return prompt, // unreachable: default is always bundled
            }
        }
    };
    inject_style_into_prompt(style, &prompt)
}

/// Convenience: resolve the active style id for a project, reading the user
/// config and gating on the live Claude Code version.
///
/// Why: the launch sites want a one-call entry point that does the real work
/// (load config, detect version, inject) without each re-implementing the
/// precedence; this wraps [`maybe_inject_active_style`] with the real config and
/// live version detection.
/// What: loads [`MpmConfig::load_default`], detects native support via
/// [`claude_supports_native_output_style`], and injects (or not) into `prompt`.
/// `_project_dir` is accepted for symmetry with the other launch helpers and to
/// allow future project-scoped style overrides; it is currently unused.
/// Test: side-effecting (loads real config, spawns `claude`); the pure core is
/// covered by `maybe_inject_*`.
pub fn apply_output_style_to_prompt(
    project_dir: &Path,
    explicit: Option<&str>,
    prompt: String,
) -> String {
    let native = claude_supports_native_output_style();
    apply_output_style_to_prompt_with_native(project_dir, explicit, prompt, native)
}

/// Apply the output-style injection decision with the `native_supported` flag
/// supplied explicitly (no live `claude --version` probe).
///
/// Why: [`apply_output_style_to_prompt`] spawns `claude --version`, which makes
/// every caller (and every test that exercises a launch-prompt path) depend on
/// whether `claude` is installed on the host — the exact host-dependence that
/// broke `prepare_session_stash_reflects_override` on CI (issue #1409). This
/// seam lets tests pin the decision BOTH ways while production still does real
/// version detection (and fails safe to injection) via the wrapper above.
/// What: loads [`MpmConfig::load_default`] and delegates to the pure
/// [`maybe_inject_active_style`] core with the caller-supplied `native_supported`
/// flag. `_project_dir` is accepted for symmetry / future project-scoped overrides.
/// Test: `output_style_tests.rs` covers the pure core directly; the
/// stash/launch invariant under both flag values is covered by
/// `prepare_session_stash_reflects_override` in `session_launch/tests.rs`.
pub fn apply_output_style_to_prompt_with_native(
    _project_dir: &Path,
    explicit: Option<&str>,
    prompt: String,
    native_supported: bool,
) -> String {
    let config = MpmConfig::load_default();
    maybe_inject_active_style(&config, explicit, prompt, native_supported)
}

#[cfg(test)]
#[path = "output_style_tests.rs"]
mod tests;
