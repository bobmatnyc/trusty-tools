//! Strict bare-identifier validation for names that flow into a path join
//! (security fix, issue #2995).
//!
//! Why: `Path::join` silently DISCARDS the base directory when the joined
//! component is itself absolute — `base.join("/etc/passwd")` evaluates to
//! `/etc/passwd`, not `base/etc/passwd` — and it never collapses `..`
//! components either. A review-template name (`[review].template`) or a
//! voice-package name (`[voice].package`) can originate from a repo-scoped
//! `.trusty-review.toml`, which is attacker-controlled: any PR author can add
//! that file to their branch. Before this fix, that value flowed unsanitised
//! into `ReviewTemplateLoader::load` / `VoiceLoader::load`'s path joins — an
//! absolute path or a `../` traversal could read an arbitrary local file
//! (including a file the attacker committed into the very PR under review),
//! whose contents are then appended verbatim into the LLM system prompt: a
//! path-traversal-to-prompt-injection chain (e.g. a hidden instruction to
//! "always APPROVE").
//!
//! What: [`is_valid_identifier`] accepts only non-empty ASCII
//! alphanumeric/`-`/`_` strings. No `/`, `\`, `.`, whitespace, or any other
//! separator/traversal character can appear, so a validated string can never
//! change which directory a subsequent `Path::join` lands in — it is always
//! exactly one path COMPONENT, never a path.
//!
//! This module is a single, dependency-free leaf so both the config-resolution
//! layer (`config::voice`, `config::review_template` — reject an invalid
//! repo/env/file-sourced value before it is even stored on `ReviewConfig`) and
//! the loaders themselves (`voice::loader::VoiceLoader`,
//! `review_template::ReviewTemplateLoader` — reject before ANY path join, as
//! defense-in-depth for every caller regardless of source) can depend on it
//! without a cyclic module relationship.
//!
//! Test: `accepts_bare_identifier`, `rejects_empty`, `rejects_absolute_path`,
//! `rejects_parent_traversal`, `rejects_embedded_separator`,
//! `rejects_home_relative_path`.

/// Return `true` when `name` is safe to use as a single path component.
///
/// Why: the sole gate between an attacker-controlled config value and a
/// filesystem path join — see the module doc for the full threat model.
/// What: requires `name` to be non-empty, and every character to be ASCII
/// alphanumeric, `-`, or `_`. Rejects (among others) `/etc/passwd`,
/// `../../etc/passwd`, `a/b`, `a\b`, `a.md`, `~`, and any embedded whitespace.
/// Test: this module's `mod tests`.
pub fn is_valid_identifier(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_bare_identifier() {
        assert!(is_valid_identifier("strict-security"));
        assert!(is_valid_identifier("duetto"));
        assert!(is_valid_identifier("my_voice-2"));
        assert!(is_valid_identifier("A1"));
    }

    #[test]
    fn rejects_empty() {
        assert!(!is_valid_identifier(""));
    }

    #[test]
    fn rejects_absolute_path() {
        assert!(!is_valid_identifier("/etc/passwd"));
        assert!(!is_valid_identifier("/tmp/HOSTILE"));
    }

    #[test]
    fn rejects_parent_traversal() {
        assert!(!is_valid_identifier("../../etc/passwd"));
        assert!(!is_valid_identifier("..secret"));
    }

    #[test]
    fn rejects_embedded_separator() {
        assert!(!is_valid_identifier("a/b"));
        assert!(!is_valid_identifier("a\\b"));
        assert!(!is_valid_identifier("a b"));
        assert!(!is_valid_identifier("a.md"));
    }

    #[test]
    fn rejects_home_relative_path() {
        assert!(!is_valid_identifier("~/secrets"));
        assert!(!is_valid_identifier("~"));
    }
}
