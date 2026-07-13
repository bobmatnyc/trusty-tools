//! Per-project language detection → agent-set scoping (#1941 / HR-2).
//!
//! Why: the compiled-in default manifest deploys the FULL polyglot agent roster
//! (every `*-engineer` — javascript/typescript/python/php/java/golang/dart/ruby/
//! svelte/react/nextjs/tauri/phoenix + rust) to every project regardless of
//! language. In a pure-Rust workspace that is pure noise in the delegation
//! surface and a contributor to the catalog-drift false positives (#1940). This
//! module auto-detects a project's language(s) from well-known marker files and
//! produces an [`AgentSet`] that scopes the deployed roster to the relevant
//! language engineers PLUS every language-agnostic agent (BASE-*, qa, ops,
//! research, generic engineers, …).
//! What: [`language_agent_scope`] probes `project_dir` for language markers
//! ([`LANGUAGE_ENGINEERS`]) and, when at least one is recognized, returns an
//! [`AgentSet`] whose `exclude` list drops exactly the language engineers that do
//! NOT match the project (keeping everything else via the empty `include`). When
//! no language marker is recognized it returns `None` so provisioning falls back
//! to today's deploy-everything behavior (zero-regression for unknown project
//! types). The scope is applied as the lowest override layer in
//! [`super::resolve::resolve_manifest`], so an explicit `[agents]` override in the
//! catalog/user/project manifest always wins.
//! Test: `rust_workspace_scopes_to_rust_engineer`,
//! `js_project_keeps_js_family`, `unknown_project_is_unscoped`,
//! `polyglot_project_keeps_both`.

use std::collections::BTreeSet;
use std::path::Path;

use super::schema::{AgentSet, ContentSource};

/// A bundled language-specific engineer agent and the marker files that select it.
///
/// Why: scoping keys the "is this engineer relevant?" decision on cheap
/// filesystem probes of conventional project root files; pairing each engineer
/// stem with its markers keeps that mapping in one auditable table.
/// What: `stem` is the agent filename without `.md`; `markers` are paths (relative
/// to the project root) whose existence implies this engineer's language/framework
/// is in use.
/// Test: exercised by every scope test below.
struct LangEngineer {
    /// Agent stem (filename without `.md`), e.g. `rust-engineer`.
    stem: &'static str,
    /// Marker paths (relative to the project root) that select this engineer.
    markers: &'static [&'static str],
}

/// The bundled language engineers and their project-root marker files.
///
/// Why: this is the single source of truth for which agents are "language
/// specific" (and thus subject to scoping) versus language-agnostic (always kept).
/// Any agent stem NOT listed here is treated as agnostic and never excluded.
/// What: one entry per bundled `*-engineer`. The JS/TS ecosystem engineers
/// (javascript/typescript/react/nextjs/svelte/tauri) all key off `package.json`
/// (plus framework-specific configs) so a JS project keeps the whole family.
/// Test: `js_project_keeps_js_family`, `rust_workspace_scopes_to_rust_engineer`.
const LANGUAGE_ENGINEERS: &[LangEngineer] = &[
    LangEngineer {
        stem: "rust-engineer",
        markers: &["Cargo.toml"],
    },
    LangEngineer {
        stem: "python-engineer",
        markers: &[
            "pyproject.toml",
            "setup.py",
            "setup.cfg",
            "requirements.txt",
            "Pipfile",
        ],
    },
    LangEngineer {
        stem: "golang-engineer",
        markers: &["go.mod"],
    },
    LangEngineer {
        stem: "java-engineer",
        markers: &[
            "pom.xml",
            "build.gradle",
            "build.gradle.kts",
            "settings.gradle",
        ],
    },
    LangEngineer {
        stem: "ruby-engineer",
        markers: &["Gemfile", "Gemfile.lock", ".ruby-version"],
    },
    LangEngineer {
        stem: "php-engineer",
        markers: &["composer.json"],
    },
    LangEngineer {
        stem: "dart-engineer",
        markers: &["pubspec.yaml"],
    },
    LangEngineer {
        stem: "phoenix-engineer",
        markers: &["mix.exs"],
    },
    LangEngineer {
        stem: "javascript-engineer",
        markers: &["package.json"],
    },
    LangEngineer {
        stem: "typescript-engineer",
        markers: &["tsconfig.json", "package.json"],
    },
    LangEngineer {
        stem: "react-engineer",
        markers: &["package.json"],
    },
    LangEngineer {
        stem: "nextjs-engineer",
        markers: &[
            "next.config.js",
            "next.config.mjs",
            "next.config.ts",
            "package.json",
        ],
    },
    LangEngineer {
        stem: "svelte-engineer",
        markers: &["svelte.config.js", "svelte.config.ts", "package.json"],
    },
    LangEngineer {
        stem: "tauri-engineer",
        markers: &["src-tauri/tauri.conf.json", "tauri.conf.json"],
    },
];

/// The set of language-engineer stems whose markers are present in `project_dir`.
///
/// Why: both the public scope function and its tests need the "which language
/// engineers are relevant here" decision in one place.
/// What: returns the stems from [`LANGUAGE_ENGINEERS`] for which at least one
/// marker file exists directly under `project_dir`. Sorted/de-duplicated via the
/// `BTreeSet`.
/// Test: `rust_workspace_scopes_to_rust_engineer`, `polyglot_project_keeps_both`.
fn detected_engineers(project_dir: &Path) -> BTreeSet<&'static str> {
    LANGUAGE_ENGINEERS
        .iter()
        .filter(|le| le.markers.iter().any(|m| project_dir.join(m).exists()))
        .map(|le| le.stem)
        .collect()
}

/// Auto-detect `project_dir`'s language(s) and scope the deployed agent roster.
///
/// Why: a Rust-only workspace should not receive the Python/JS/Java/etc. engineers
/// (#1941). Detecting the project's language at resolve time and dropping the
/// irrelevant language engineers keeps the delegation surface focused while never
/// touching language-agnostic agents.
/// What: probes `project_dir` for the [`LANGUAGE_ENGINEERS`] markers. If none
/// match (unknown project type — e.g. the daemon's framework root), returns `None`
/// so provisioning deploys everything (unchanged behavior). Otherwise returns an
/// [`AgentSet`] with an empty `include` (= keep all) and an `exclude` naming every
/// language engineer that did NOT match — so BASE-*, generic roles, and the
/// project's own language engineers survive while foreign-language engineers are
/// dropped. `source` is [`ContentSource::Bundled`], matching the default.
/// Test: `rust_workspace_scopes_to_rust_engineer`, `unknown_project_is_unscoped`,
/// `js_project_keeps_js_family`, `polyglot_project_keeps_both`.
pub fn language_agent_scope(project_dir: &Path) -> Option<AgentSet> {
    let kept = detected_engineers(project_dir);
    if kept.is_empty() {
        // No recognizable language markers → do not scope (deploy everything).
        return None;
    }

    let mut exclude: Vec<String> = LANGUAGE_ENGINEERS
        .iter()
        .map(|le| le.stem)
        .filter(|stem| !kept.contains(stem))
        .map(str::to_owned)
        .collect();
    exclude.sort_unstable();
    exclude.dedup();

    Some(AgentSet {
        include: Vec::new(),
        exclude,
        ignore_staleness: Vec::new(),
        source: ContentSource::Bundled,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn touch(dir: &Path, name: &str) {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, "").unwrap();
    }

    #[test]
    fn rust_workspace_scopes_to_rust_engineer() {
        // A Cargo workspace must keep rust-engineer and exclude every other
        // language engineer.
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "Cargo.toml");

        let scope = language_agent_scope(tmp.path()).expect("rust project is scoped");
        assert!(
            scope.include.is_empty(),
            "empty include = keep all agnostic"
        );
        assert!(
            !scope.exclude.contains(&"rust-engineer".to_string()),
            "rust-engineer must be kept"
        );
        for foreign in [
            "python-engineer",
            "javascript-engineer",
            "typescript-engineer",
            "golang-engineer",
            "java-engineer",
            "ruby-engineer",
            "php-engineer",
            "dart-engineer",
            "phoenix-engineer",
            "react-engineer",
            "nextjs-engineer",
            "svelte-engineer",
            "tauri-engineer",
        ] {
            assert!(
                scope.exclude.contains(&foreign.to_string()),
                "{foreign} must be excluded from a Rust-only project"
            );
        }
        assert_eq!(scope.source, ContentSource::Bundled);
    }

    #[test]
    fn unknown_project_is_unscoped() {
        // A directory with no recognized language marker must NOT be scoped, so
        // provisioning falls back to deploy-everything (zero regression).
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "README.md");
        assert!(
            language_agent_scope(tmp.path()).is_none(),
            "unknown project type must not be scoped"
        );
    }

    #[test]
    fn js_project_keeps_js_family() {
        // A package.json project keeps the whole JS/TS family and drops the
        // non-JS engineers (rust/python/…).
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "package.json");

        let scope = language_agent_scope(tmp.path()).expect("js project is scoped");
        for kept in [
            "javascript-engineer",
            "typescript-engineer",
            "react-engineer",
            "nextjs-engineer",
            "svelte-engineer",
        ] {
            assert!(
                !scope.exclude.contains(&kept.to_string()),
                "{kept} must be kept for a package.json project"
            );
        }
        assert!(scope.exclude.contains(&"rust-engineer".to_string()));
        assert!(scope.exclude.contains(&"python-engineer".to_string()));
    }

    #[test]
    fn polyglot_project_keeps_both() {
        // A repo with both Cargo.toml and pyproject.toml keeps rust + python and
        // drops the rest.
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "Cargo.toml");
        touch(tmp.path(), "pyproject.toml");

        let scope = language_agent_scope(tmp.path()).expect("polyglot is scoped");
        assert!(!scope.exclude.contains(&"rust-engineer".to_string()));
        assert!(!scope.exclude.contains(&"python-engineer".to_string()));
        assert!(scope.exclude.contains(&"golang-engineer".to_string()));
    }

    #[test]
    fn tauri_marker_scopes_without_bare_package_json() {
        // A Tauri config selects tauri-engineer even though tauri.conf.json lives
        // under src-tauri/.
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "src-tauri/tauri.conf.json");
        let scope = language_agent_scope(tmp.path()).expect("tauri project is scoped");
        assert!(
            !scope.exclude.contains(&"tauri-engineer".to_string()),
            "tauri-engineer must be kept"
        );
    }
}
