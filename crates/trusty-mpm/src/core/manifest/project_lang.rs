//! Per-project stack and platform MARKER DETECTION (#1941 / HR-2, #4760).
//!
//! Why: deploying the FULL polyglot agent roster (every `*-engineer` —
//! javascript/typescript/python/php/java/golang/dart/ruby/svelte/react/nextjs/
//! tauri/phoenix/dotnet + rust) plus every platform-ops agent to every project
//! regardless of stack is pure noise in the delegation surface and a contributor
//! to the catalog-drift false positives (#1940). This module answers the two
//! detection questions the framework manifest's gated categories depend on:
//! *which stacks does this project use* and *which platforms does it target*.
//! What: [`detected_engineers`] probes `project_dir` for the
//! [`LANGUAGE_ENGINEERS`] markers; [`detected_platforms`] probes it for the
//! [`PLATFORM_AGENTS`] markers. Both return a possibly-EMPTY set — an empty
//! result is a valid answer, never an error. `super::framework` composes those
//! answers with the framework manifest's declared categories into the final
//! [`super::schema::AgentSet`]; this module owns markers only, never selection
//! policy. (#4760 replaced this module's former `language_agent_scope`, which
//! computed an exclude-list by complement, with that declarative composition.)
//! Test: `rust_workspace_detects_only_rust_engineer`, `js_project_detects_js_family`,
//! `unknown_project_detects_nothing`, `vercel_marker_detects_vercel_ops`,
//! `gcp_marker_detects_gcp_ops`, `no_platform_marker_detects_nothing`.

use std::collections::BTreeSet;
use std::path::Path;

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
        stem: "dotnet-engineer",
        // C#/.NET and legacy VB.NET: project/solution files are extension globs
        // (`*.csproj`/`*.sln`/`*.vbproj`), matched by `marker_present`'s `*.<ext>`
        // support; `global.json` and `Directory.Build.props` are exact filenames.
        markers: &[
            "*.sln",
            "*.csproj",
            "*.vbproj",
            "global.json",
            "Directory.Build.props",
        ],
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

/// Whether a single marker (exact path or `*.<ext>` glob) is present in `project_dir`.
///
/// Why: most language markers are fixed filenames, but .NET projects are
/// identified by extension globs — `*.csproj`, `*.sln`, `*.vbproj` — that no
/// single fixed name captures, so marker matching must support a leading `*.`
/// extension glob in addition to exact-path probes.
/// What: for a marker of the form `*.<ext>` returns `true` when any direct child
/// file of `project_dir` ends with `.<ext>`; otherwise tests `project_dir.join(marker)`
/// for existence. A failed directory read (missing/unreadable dir) yields `false`.
/// Test: `dotnet_csproj_scopes_to_dotnet_engineer` (glob branch); every other
/// scope test exercises the exact-path branch.
fn marker_present(project_dir: &Path, marker: &str) -> bool {
    if let Some(ext) = marker.strip_prefix("*.") {
        let suffix = format!(".{ext}");
        std::fs::read_dir(project_dir)
            .map(|entries| {
                entries
                    .flatten()
                    .any(|e| e.file_name().to_str().is_some_and(|n| n.ends_with(&suffix)))
            })
            .unwrap_or(false)
    } else {
        project_dir.join(marker).exists()
    }
}

/// The set of language-engineer stems whose markers are present in `project_dir`.
///
/// Why: both the public scope function and its tests need the "which language
/// engineers are relevant here" decision in one place.
/// What: returns the stems from [`LANGUAGE_ENGINEERS`] for which at least one
/// marker file exists directly under `project_dir`. Sorted/de-duplicated via the
/// `BTreeSet`.
/// Test: `rust_workspace_scopes_to_rust_engineer`, `polyglot_project_keeps_both`.
pub(crate) fn detected_engineers(project_dir: &Path) -> BTreeSet<&'static str> {
    LANGUAGE_ENGINEERS
        .iter()
        .filter(|le| le.markers.iter().any(|m| marker_present(project_dir, m)))
        .map(|le| le.stem)
        .collect()
}

/// A bundled platform-ops agent and the marker files that select it (#4760).
///
/// Why: `gcp-ops` and `vercel-ops` are gated on a detected PLATFORM, which is a
/// third axis alongside language and framework — a Rust API deployed to Vercel
/// is neither a "JavaScript project" nor a "Next.js project", so
/// [`LANGUAGE_ENGINEERS`] cannot express the gate. Before #4760 they were
/// ungated and deployed to every project.
/// What: same shape as [`LangEngineer`], probed by the same [`marker_present`].
/// Test: `vercel_marker_detects_vercel_ops`, `gcp_marker_detects_gcp_ops`,
/// `no_platform_marker_detects_nothing`.
struct PlatformAgent {
    /// Agent stem (filename without `.md`), e.g. `vercel-ops`.
    stem: &'static str,
    /// Marker paths (relative to the project root) that select this agent.
    markers: &'static [&'static str],
}

/// The bundled platform-ops agents and their project-root marker files.
///
/// Why: the single source of truth for which agents are "platform specific".
/// The marker sets are deliberately narrow — each entry is a file the platform's
/// own tooling creates or requires, not an inferred heuristic — because a false
/// positive puts an irrelevant agent in every roster, which is the exact noise
/// #1941 built language scoping to remove.
/// What: one entry per bundled `*-ops` agent that targets a single cloud
/// platform. `local-ops` is deliberately absent: it is platform-agnostic and
/// stays universal.
/// Test: `vercel_marker_detects_vercel_ops`, `gcp_marker_detects_gcp_ops`.
const PLATFORM_AGENTS: &[PlatformAgent] = &[
    PlatformAgent {
        stem: "gcp-ops",
        // `app.yaml` (App Engine), `cloudbuild.yaml`/`.yml` (Cloud Build), and
        // `.gcloudignore` (every `gcloud` deploy) are created by GCP tooling.
        markers: &[
            "app.yaml",
            "cloudbuild.yaml",
            "cloudbuild.yml",
            ".gcloudignore",
        ],
    },
    PlatformAgent {
        stem: "vercel-ops",
        // `vercel.json` is the project config; `.vercel/project.json` is written
        // by `vercel link`; `.vercelignore` by the deploy flow.
        markers: &["vercel.json", ".vercelignore", ".vercel/project.json"],
    },
];

/// The set of platform-agent stems whose markers are present in `project_dir`.
///
/// Why: the platform category needs the same "which of these are relevant here"
/// probe the language/framework categories get from [`detected_engineers`].
/// What: returns the stems from [`PLATFORM_AGENTS`] for which at least one
/// marker exists under `project_dir`. An EMPTY result is a valid, expected
/// answer meaning "this project targets no known platform" — it is never
/// confused with a manifest error, which is a distinct `Err` on a different
/// code path (`core::manifest::framework::parse_framework_manifest`).
/// Test: `no_platform_marker_detects_nothing`, `vercel_marker_detects_vercel_ops`.
pub(crate) fn detected_platforms(project_dir: &Path) -> BTreeSet<&'static str> {
    PLATFORM_AGENTS
        .iter()
        .filter(|pa| pa.markers.iter().any(|m| marker_present(project_dir, m)))
        .map(|pa| pa.stem)
        .collect()
}

/// Every stem [`LANGUAGE_ENGINEERS`] carries markers for.
///
/// Why: the framework manifest declares which stems are language- or
/// framework-gated; that declaration is only meaningful if each declared stem
/// actually HAS a marker row here. Exposing the stem set lets
/// `core::manifest::framework` enforce that as a loud invariant instead of
/// letting a typo silently produce an agent that can never deploy.
/// What: the `stem` column of [`LANGUAGE_ENGINEERS`].
/// Test: `language_and_framework_stems_all_have_markers`.
pub(crate) fn language_engineer_stems() -> BTreeSet<&'static str> {
    LANGUAGE_ENGINEERS.iter().map(|le| le.stem).collect()
}

/// Every stem [`PLATFORM_AGENTS`] carries markers for.
///
/// Why/What/Test: as [`language_engineer_stems`], for the platform table.
pub(crate) fn platform_agent_stems() -> BTreeSet<&'static str> {
    PLATFORM_AGENTS.iter().map(|pa| pa.stem).collect()
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
    fn rust_workspace_detects_only_rust_engineer() {
        // A Cargo workspace must detect rust-engineer and NO other language
        // engineer.
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "Cargo.toml");

        let detected = detected_engineers(tmp.path());
        assert!(detected.contains("rust-engineer"), "rust must be detected");
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
            "dotnet-engineer",
        ] {
            assert!(
                !detected.contains(foreign),
                "{foreign} must not be detected in a Rust-only project"
            );
        }
    }

    #[test]
    fn unknown_project_detects_nothing() {
        // A directory with no recognized language marker detects nothing — the
        // signal `framework::agent_scope_from` reads as "unknown project type".
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "README.md");
        assert!(detected_engineers(tmp.path()).is_empty());
    }

    #[test]
    fn js_project_detects_js_family() {
        // A package.json project detects the whole JS/TS family and no non-JS
        // engineer. This pins TODAY'S marker behavior: react/nextjs/svelte fire
        // on a bare package.json, with no framework-specific marker required.
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "package.json");

        let detected = detected_engineers(tmp.path());
        for kept in [
            "javascript-engineer",
            "typescript-engineer",
            "react-engineer",
            "nextjs-engineer",
            "svelte-engineer",
        ] {
            assert!(
                detected.contains(kept),
                "{kept} must be detected for a package.json project"
            );
        }
        assert!(!detected.contains("rust-engineer"));
        assert!(!detected.contains("python-engineer"));
    }

    #[test]
    fn polyglot_project_detects_both() {
        // A repo with both Cargo.toml and pyproject.toml detects rust + python
        // and nothing else.
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "Cargo.toml");
        touch(tmp.path(), "pyproject.toml");

        let detected = detected_engineers(tmp.path());
        assert!(detected.contains("rust-engineer"));
        assert!(detected.contains("python-engineer"));
        assert!(!detected.contains("golang-engineer"));
    }

    #[test]
    fn dotnet_csproj_detects_dotnet_engineer() {
        // A .NET project is identified by an extension-glob marker (`*.csproj`),
        // exercising `marker_present`'s glob branch.
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "MyApp.csproj");

        let detected = detected_engineers(tmp.path());
        assert!(detected.contains("dotnet-engineer"));
        assert!(!detected.contains("rust-engineer"));
        assert!(!detected.contains("golang-engineer"));
    }

    #[test]
    fn dotnet_vbproj_and_exact_markers_detect_dotnet_engineer() {
        // Legacy VB.NET (`*.vbproj` glob) and the exact-filename markers
        // (`global.json`, `Directory.Build.props`) all select dotnet-engineer.
        for marker in ["Legacy.vbproj", "global.json", "Directory.Build.props"] {
            let tmp = TempDir::new().unwrap();
            touch(tmp.path(), marker);
            assert!(
                detected_engineers(tmp.path()).contains("dotnet-engineer"),
                "dotnet-engineer must be detected for a {marker} project"
            );
        }
    }

    #[test]
    fn tauri_marker_detects_without_bare_package_json() {
        // A Tauri config selects tauri-engineer even though tauri.conf.json
        // lives under src-tauri/ — and, unlike react/nextjs/svelte, tauri has NO
        // package.json fallback, so it is the one genuinely framework-gated stem.
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "src-tauri/tauri.conf.json");
        let detected = detected_engineers(tmp.path());
        assert!(detected.contains("tauri-engineer"));

        let bare_js = TempDir::new().unwrap();
        touch(bare_js.path(), "package.json");
        assert!(
            !detected_engineers(bare_js.path()).contains("tauri-engineer"),
            "tauri must NOT fire on a bare package.json"
        );
    }

    #[test]
    fn vercel_marker_detects_vercel_ops() {
        // Each Vercel marker selects vercel-ops and nothing else.
        for marker in ["vercel.json", ".vercelignore", ".vercel/project.json"] {
            let tmp = TempDir::new().unwrap();
            touch(tmp.path(), marker);
            let detected = detected_platforms(tmp.path());
            assert!(
                detected.contains("vercel-ops"),
                "vercel-ops must be detected for {marker}"
            );
            assert!(
                !detected.contains("gcp-ops"),
                "gcp-ops must NOT be detected for {marker}"
            );
        }
    }

    #[test]
    fn gcp_marker_detects_gcp_ops() {
        // Each GCP marker selects gcp-ops and nothing else.
        for marker in [
            "app.yaml",
            "cloudbuild.yaml",
            "cloudbuild.yml",
            ".gcloudignore",
        ] {
            let tmp = TempDir::new().unwrap();
            touch(tmp.path(), marker);
            let detected = detected_platforms(tmp.path());
            assert!(
                detected.contains("gcp-ops"),
                "gcp-ops must be detected for {marker}"
            );
            assert!(!detected.contains("vercel-ops"));
        }
    }

    #[test]
    fn no_platform_marker_detects_nothing() {
        // A project with language markers but no platform marker detects zero
        // platforms — an explicit, valid empty answer, never an error.
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "Cargo.toml");
        touch(tmp.path(), "package.json");
        assert!(
            detected_platforms(tmp.path()).is_empty(),
            "no platform marker → empty platform set"
        );
        assert!(
            !detected_engineers(tmp.path()).is_empty(),
            "the same project DOES detect languages — so the empty platform set \
             cannot be an artifact of an unreadable directory"
        );
    }

    #[test]
    fn stem_accessors_match_their_tables() {
        assert_eq!(language_engineer_stems().len(), LANGUAGE_ENGINEERS.len());
        assert_eq!(platform_agent_stems().len(), PLATFORM_AGENTS.len());
        assert!(platform_agent_stems().contains("gcp-ops"));
        assert!(platform_agent_stems().contains("vercel-ops"));
    }
}
