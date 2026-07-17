//! Content-aware delegation routing hints for `tm hook --pm-guard` denial
//! messages (Bob's directive, 2026-07-17; see issue #2918).
//!
//! Why: the pre-existing deny reasons (`SOURCE_EDIT_REASON` in `pm_guard.rs`,
//! `SHELL_EDIT_REASON` in `pm_guard_bash`) hardcoded "Delegate … to
//! rust-engineer" regardless of what kind of file the PM was actually trying
//! to touch. That is misleading for every non-Rust target — a blocked `.md`
//! edit routed to rust-engineer, a blocked `.py` edit too. The target path
//! (already resolved by the caller — [`super::pm_guard::edit_tool_target_path`]
//! for Edit/Write/MultiEdit/NotebookEdit, or a best-effort extraction from the
//! Bash command for shell-based edits) is a cheap, reliable signal for which
//! specialist should actually receive the delegated work.
//! What: [`delegation_hint_for_path`] classifies a target path into a short
//! phrase — `"the documentation agent"` for `.md`/`.mdx`/`.rst`/`.adoc` files
//! or any path under a `docs/` directory, a specific `<stack>-engineer` name
//! for the mainstream languages with a dedicated bundled agent, and the
//! generic `"the appropriate engineer agent"` for everything else (including
//! extension-less files and languages with no dedicated stack agent). Callers
//! splice the returned phrase into their own reason template.
//! Test: `delegation_hint_for_path_*` below.

/// Generic fallback hint when no specific agent mapping applies.
///
/// Why: a language/extension outside [`STACK_AGENTS`] still deserves an
/// actionable hint rather than a wrong one (the old hardcoded rust-engineer).
/// What: returned by [`delegation_hint_for_path`] for extension-less paths and
/// extensions not present in [`STACK_AGENTS`].
pub(crate) const GENERIC_ENGINEER_HINT: &str = "the appropriate engineer agent";

/// Hint routed to the documentation specialist.
///
/// What: returned by [`delegation_hint_for_path`] for doc-shaped paths — see
/// its doc comment for the exact match rules.
const DOCUMENTATION_HINT: &str = "the documentation agent";

/// Lowercase (no-dot) file extension → bundled stack-engineer agent name.
///
/// Why: mirrors `pm_guard::SOURCE_CODE_EXTENSIONS` in spirit but maps to an
/// actual delegate rather than merely classifying "is this source code" — only
/// extensions with a real bundled stack-engineer agent are listed here
/// (matching the trusty-mpm agent catalog); everything else falls back to
/// [`GENERIC_ENGINEER_HINT`] per Bob's "cheap mapping, else generic" directive.
/// What: consulted by [`delegation_hint_for_path`]; first match wins (the list
/// has no duplicate keys).
const STACK_AGENTS: &[(&str, &str)] = &[
    ("rs", "rust-engineer"),
    ("py", "python-engineer"),
    ("pyi", "python-engineer"),
    ("pyx", "python-engineer"),
    ("ipynb", "python-engineer"),
    ("ts", "typescript-engineer"),
    ("tsx", "typescript-engineer"),
    ("js", "javascript-engineer"),
    ("jsx", "javascript-engineer"),
    ("mjs", "javascript-engineer"),
    ("cjs", "javascript-engineer"),
    ("go", "golang-engineer"),
    ("java", "java-engineer"),
    ("kt", "java-engineer"),
    ("kts", "java-engineer"),
    ("rb", "ruby-engineer"),
    ("php", "php-engineer"),
    ("cs", "dotnet-engineer"),
    ("dart", "dart-engineer"),
    ("ex", "phoenix-engineer"),
    ("exs", "phoenix-engineer"),
    ("svelte", "svelte-engineer"),
    ("vue", "web-ui-engineer"),
    ("html", "web-ui-engineer"),
    ("css", "web-ui-engineer"),
];

/// Doc-shaped file extensions (lowercase, no dot) that route to the
/// documentation agent regardless of directory.
const DOC_EXTENSIONS: &[&str] = &["md", "mdx", "rst", "adoc"];

/// Classify a target path into a delegation-hint phrase for a denial message.
///
/// Why: see the module doc — the goal is to suggest the specialist that
/// actually owns the target file's content, not a hardcoded default.
/// What: doc-shaped first (any `docs/`-rooted path, or a
/// [`DOC_EXTENSIONS`] extension, regardless of directory) →
/// [`DOCUMENTATION_HINT`]; else a [`STACK_AGENTS`] extension match → that
/// agent's name; else [`GENERIC_ENGINEER_HINT`]. Matching is
/// case-insensitive on the extension; the `docs/` directory check matches a
/// path *component* named exactly `docs` (not a substring like `adocs/`).
/// Test: `delegation_hint_for_path_routes_docs`,
/// `delegation_hint_for_path_routes_known_stacks`,
/// `delegation_hint_for_path_falls_back_generic`.
pub(crate) fn delegation_hint_for_path(path: &str) -> &'static str {
    use std::path::{Component, Path};

    let p = Path::new(path);
    let ext = p
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());

    if let Some(ext) = ext.as_deref()
        && DOC_EXTENSIONS.contains(&ext)
    {
        return DOCUMENTATION_HINT;
    }
    let in_docs_dir = p
        .components()
        .any(|c| matches!(c, Component::Normal(os) if os.to_str() == Some("docs")));
    if in_docs_dir {
        return DOCUMENTATION_HINT;
    }
    if let Some(ext) = ext.as_deref()
        && let Some((_, agent)) = STACK_AGENTS.iter().find(|(e, _)| *e == ext)
    {
        return agent;
    }
    GENERIC_ENGINEER_HINT
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delegation_hint_for_path_routes_docs() {
        for p in [
            "README.md",
            "docs/reference/foo.md",
            "notes.mdx",
            "spec.rst",
            "docs/provenance/note.txt", // docs/-rooted, non-doc extension still routes to docs
        ] {
            assert_eq!(
                delegation_hint_for_path(p),
                "the documentation agent",
                "{p} should route to the documentation agent"
            );
        }
    }

    #[test]
    fn delegation_hint_for_path_routes_known_stacks() {
        assert_eq!(
            delegation_hint_for_path("crates/foo/src/lib.rs"),
            "rust-engineer"
        );
        assert_eq!(delegation_hint_for_path("app/main.py"), "python-engineer");
        assert_eq!(
            delegation_hint_for_path("web/App.tsx"),
            "typescript-engineer"
        );
        assert_eq!(
            delegation_hint_for_path("web/index.js"),
            "javascript-engineer"
        );
        assert_eq!(delegation_hint_for_path("cmd/main.go"), "golang-engineer");
        assert_eq!(delegation_hint_for_path("src/Main.java"), "java-engineer");
        assert_eq!(delegation_hint_for_path("app/user.rb"), "ruby-engineer");
        assert_eq!(delegation_hint_for_path("app/User.php"), "php-engineer");
        assert_eq!(delegation_hint_for_path("Program.cs"), "dotnet-engineer");
        assert_eq!(delegation_hint_for_path("lib/main.dart"), "dart-engineer");
        assert_eq!(
            delegation_hint_for_path("web/App.svelte"),
            "svelte-engineer"
        );
    }

    #[test]
    fn delegation_hint_for_path_falls_back_generic() {
        for p in ["Makefile", "README", "a.swift", "a.scala", "a.sql", "a.c"] {
            assert_eq!(
                delegation_hint_for_path(p),
                GENERIC_ENGINEER_HINT,
                "{p} should fall back to the generic hint"
            );
        }
    }

    #[test]
    fn delegation_hint_for_path_docs_dir_component_not_substring() {
        // "adocs/" must NOT match the `docs` component check.
        assert_ne!(
            delegation_hint_for_path("adocs/file.rs"),
            "the documentation agent"
        );
        assert_eq!(delegation_hint_for_path("adocs/file.rs"), "rust-engineer");
    }
}
