//! The agent-name allowlist every assistant-config path joins against.
//!
//! #11632: what is left of `api::server::agent_stores` after the HTTP route
//! and its store report were cut with the API.

/// Maximum accepted agent-name length. Matches `agent_create::is_valid_slug`'s
/// cap, so a name this route accepts is one the create path could have written.
const MAX_AGENT_NAME_LEN: usize = 64;

/// Is `name` a safe single path segment to join onto an agents directory?
///
/// Why: this used to be a DENYLIST — `/`, `\`, `.`, `..` (audit 2026-08-19,
/// finding 12). A denylist has to anticipate every hostile spelling, and the
/// name is joined straight onto a filesystem path by `resolve_agent_paths`.
/// It admitted NUL bytes, control characters, whitespace-only names,
/// unbounded lengths, and Unicode look-alikes for the separators it did
/// reject (`＼` U+FF3C, `∕` U+2215, `․` U+2024) — every one of which reaches
/// the join. An allowlist inverts the burden: only characters that can never
/// change a path's meaning get through, so an unanticipated spelling fails
/// closed.
/// What: non-empty, at most [`MAX_AGENT_NAME_LEN`] bytes, and every character
/// is ASCII alphanumeric, `_`, or `-`. That admits every bundled agent name
/// (see `agent_name_allowlist_accepts_every_bundled_agent`) and every name
/// `POST /api/agents` can create.
/// Test: `agent_name_allowlist_rejects_traversal_and_unicode_tricks`,
/// `agent_name_allowlist_accepts_every_bundled_agent`.
pub(crate) fn is_valid_agent_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_AGENT_NAME_LEN
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    /// audit 2026-08-19 (finding 12): the name reaches `dir.join(name)`, so
    /// anything that can alter a path's meaning must fail closed.
    ///
    /// Why: the previous denylist named four spellings and let everything else
    /// through. These cases are the ones it missed — Unicode separator
    /// look-alikes, percent-encoded traversal, NUL and control bytes,
    /// whitespace-only names, and an unbounded length — alongside the four it
    /// did catch, so neither class can regress.
    /// Test: itself.
    #[test]
    fn agent_name_allowlist_rejects_traversal_and_unicode_tricks() {
        let rejected = [
            // What the old denylist already caught.
            "",
            ".",
            "..",
            "../etc/passwd",
            "a/b",
            "a\\b",
            // What it did not.
            "\u{FF3C}etc",      // fullwidth reverse solidus
            "a\u{2215}b",       // division slash
            "a\u{2044}b",       // fraction slash
            "a\u{2024}b",       // one-dot leader
            "..%2fetc",         // percent-encoded traversal
            "%2e%2e%2fetc",     // fully encoded `../`
            "izzie\0",          // NUL byte
            "izzie\n",          // control character
            "izzie ",           // trailing space
            "   ",              // whitespace only
            "izzie.toml",       // extension smuggling
            "cto\u{2011}agent", // non-breaking hyphen
            "\u{0430}ssistant", // Cyrillic а homoglyph
        ];
        for name in rejected {
            assert!(
                !is_valid_agent_name(name),
                "must reject {name:?} — it is joined onto a filesystem path"
            );
        }
        assert!(
            !is_valid_agent_name(&"a".repeat(MAX_AGENT_NAME_LEN + 1)),
            "length must be bounded"
        );
        assert!(is_valid_agent_name(&"a".repeat(MAX_AGENT_NAME_LEN)));
    }
}
