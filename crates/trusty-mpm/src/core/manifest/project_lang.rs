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
use std::path::{Path, PathBuf};

use super::workspace::{ProbeBudget, probe_roots};

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
        stem: "elixir-engineer",
        // `mix.exs` is the Elixir project manifest — every Mix project has one.
        // Before #4760 this marker selected `phoenix-engineer`, which meant a
        // plain Elixir project got a Phoenix specialist and nothing else.
        markers: &["mix.exs"],
    },
    LangEngineer {
        stem: "phoenix-engineer",
        // #4760: narrowed from the bare `mix.exs` Elixir marker. Phoenix has no
        // config file of its own that plain Elixir lacks, so the only reliable
        // signal is the dependency declaration itself. `{:phoenix,` is the
        // canonical `mix format` spelling of the dep tuple and does not match
        // `{:phoenix_live_view,` or any other `phoenix_*` package.
        markers: &["mix.exs::{:phoenix,"],
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
        // #4760: narrowed from a bare `package.json`, which selected React for
        // every JavaScript project. React ships no config file of its own, so
        // the dependency declaration is the only reliable signal. The quotes
        // make it an exact key match: `"react"` does not match `"react-dom"`,
        // `"react-native"`, or `"@types/react"`.
        markers: &["package.json::\"react\""],
    },
    LangEngineer {
        stem: "nextjs-engineer",
        // #4760: the `package.json` fallback is gone. The config file is the
        // primary signal, but `next.config.*` is OPTIONAL in Next.js, so the
        // dependency declaration backstops it. `"next"` does not match
        // `"next-auth"` or `"next-themes"`.
        markers: &[
            "next.config.js",
            "next.config.mjs",
            "next.config.ts",
            "package.json::\"next\"",
        ],
    },
    LangEngineer {
        stem: "svelte-engineer",
        // #4760: the `package.json` fallback is gone. `svelte.config.*` is
        // present in every SvelteKit app and most plain Svelte ones; the dep
        // declaration backstops the rest. `"svelte"` does not match
        // `"svelte-check"` or `"@sveltejs/kit"`.
        markers: &[
            "svelte.config.js",
            "svelte.config.ts",
            "package.json::\"svelte\"",
        ],
    },
    LangEngineer {
        stem: "tauri-engineer",
        markers: &["src-tauri/tauri.conf.json", "tauri.conf.json"],
    },
];

/// Whether a single marker is present in `dir`.
///
/// Why: three marker kinds are needed, because three kinds of project actually
/// exist. Most stacks are identified by a fixed filename. .NET projects are
/// identified by extension globs (`*.csproj`, `*.sln`, `*.vbproj`) that no fixed
/// name captures. And React and Phoenix (#4760) are identified by NEITHER — React
/// ships no config file at all, and Phoenix ships nothing that plain Elixir
/// lacks, so for those the dependency declaration inside the project manifest is
/// the only reliable signal.
/// What: three forms, in precedence order.
///
/// * `<path>::<needle>` — a CONTENT PROBE. True when `<path>` exists, is within
///   the size cap, is affordable under `budget`, and contains `<needle>` as a
///   literal substring. This is a deliberate, bounded read of a declaration the
///   project wrote about itself — not an inference. Needles are written with
///   their surrounding syntax (`"react"` with quotes, `{:phoenix,` with the
///   tuple brace) so a substring match is an exact declaration match and cannot
///   catch a longer-named sibling package.
/// * `*.<ext>` — an extension glob over the direct children of `dir`.
/// * anything else — an exact path, tested for existence.
///
/// Any I/O failure (missing or unreadable path, non-UTF-8 contents) or an
/// exhausted budget yields `false`: a marker that cannot be read is a marker
/// that is not present.
/// Test: `content_probe_matches_a_declared_dependency`,
/// `content_probe_does_not_match_a_longer_named_sibling`,
/// `content_probe_ignores_an_oversized_file`,
/// `dotnet_csproj_detects_dotnet_engineer` (glob branch); every other detection
/// test exercises the exact-path branch.
fn marker_present(dir: &Path, marker: &str, budget: &ProbeBudget) -> bool {
    if let Some((path, needle)) = marker.split_once("::") {
        return file_contains(&dir.join(path), needle, budget);
    }
    if let Some(ext) = marker.strip_prefix("*.") {
        let suffix = format!(".{ext}");
        return std::fs::read_dir(dir)
            .map(|entries| {
                entries
                    .flatten()
                    .any(|e| e.file_name().to_str().is_some_and(|n| n.ends_with(&suffix)))
            })
            .unwrap_or(false);
    }
    dir.join(marker).exists()
}

/// Whether `path` is a readable, size- and budget-bounded file containing `needle`.
///
/// Why: factored out so the size guard, the shared budget, and the
/// failure-is-absence rule are stated once and testable on their own.
/// What: delegates the bounded read to [`super::workspace::read_bounded`], then
/// tests for the literal substring.
/// Test: `content_probe_matches_a_declared_dependency`,
/// `content_probe_ignores_an_oversized_file`.
fn file_contains(path: &Path, needle: &str, budget: &ProbeBudget) -> bool {
    super::workspace::read_bounded(path, budget).is_some_and(|body| body.contains(needle))
}

/// Whether `marker` is present at the project root or at any workspace member.
///
/// Why (#4760 monorepo regression): probing only the project root loses every
/// marker a monorepo keeps inside its members — a turborepo root declares
/// neither `"react"` nor `"next"` and carries no `next.config.*`, so a Next.js
/// monorepo would lose three engineers it had before the gates tightened. The
/// gate narrowing was authorised for single-package projects that never declared
/// the framework, not for monorepos that declare it one directory down.
/// What: `true` when [`marker_present`] holds for ANY entry in `roots` (the
/// project root first, then each declared workspace member — see
/// [`super::workspace::probe_roots`]).
/// Test: `npm_monorepo_member_declares_react`,
/// `elixir_umbrella_member_declares_phoenix`,
/// `monorepo_without_the_framework_still_misses_it`.
fn marker_present_in_any(roots: &[PathBuf], marker: &str, budget: &ProbeBudget) -> bool {
    roots
        .iter()
        .any(|root| marker_present(root, marker, budget))
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
    let budget = ProbeBudget::new();
    let roots = probe_roots(project_dir, &budget);
    LANGUAGE_ENGINEERS
        .iter()
        .filter(|le| {
            le.markers
                .iter()
                .any(|m| marker_present_in_any(&roots, m, &budget))
        })
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
    let budget = ProbeBudget::new();
    let roots = probe_roots(project_dir, &budget);
    PLATFORM_AGENTS
        .iter()
        .filter(|pa| {
            pa.markers
                .iter()
                .any(|m| marker_present_in_any(&roots, m, &budget))
        })
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
        write(dir, name, "");
    }

    fn write(dir: &Path, name: &str, body: &str) {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, body).unwrap();
    }

    /// A `package.json` declaring exactly `deps` — the fixture the tightened
    /// JS-framework gates are written against.
    fn package_json(deps: &[&str]) -> String {
        let entries: Vec<String> = deps
            .iter()
            .map(|d| format!("    \"{d}\": \"1.0.0\""))
            .collect();
        format!(
            "{{\n  \"dependencies\": {{\n{}\n  }}\n}}\n",
            entries.join(",\n")
        )
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
            "elixir-engineer",
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
    fn bare_js_project_detects_no_framework_engineer() {
        // #4760 behavior change: a plain `package.json` with no framework
        // dependency selects the LANGUAGE engineers only. Before #4760 it also
        // selected react/nextjs/svelte, which is the accident this closes.
        let tmp = TempDir::new().unwrap();
        write(tmp.path(), "package.json", &package_json(&["lodash"]));

        let detected = detected_engineers(tmp.path());
        assert!(detected.contains("javascript-engineer"));
        assert!(detected.contains("typescript-engineer"));
        for framework in ["react-engineer", "nextjs-engineer", "svelte-engineer"] {
            assert!(
                !detected.contains(framework),
                "{framework} must NOT fire on a framework-free package.json"
            );
        }
        assert!(!detected.contains("rust-engineer"));
        assert!(!detected.contains("python-engineer"));
    }

    #[test]
    fn react_dependency_detects_react_engineer() {
        // The declared dependency is the signal, and it is exact.
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "package.json",
            &package_json(&["react", "react-dom"]),
        );
        let detected = detected_engineers(tmp.path());
        assert!(detected.contains("react-engineer"));
        assert!(
            detected.contains("javascript-engineer"),
            "language engineers still fire"
        );
        assert!(!detected.contains("nextjs-engineer"));
        assert!(!detected.contains("svelte-engineer"));
    }

    #[test]
    fn content_probe_does_not_match_a_longer_named_sibling() {
        // `"react-dom"`/`"react-native"`/`"@types/react"` must NOT satisfy the
        // `"react"` probe — the quotes make it an exact key match. This is the
        // assertion that makes the probe a declaration match, not a heuristic.
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "package.json",
            &package_json(&["react-dom", "react-native", "@types/react"]),
        );
        assert!(!detected_engineers(tmp.path()).contains("react-engineer"));

        // Same rule on the other three probes.
        let nx = TempDir::new().unwrap();
        write(
            nx.path(),
            "package.json",
            &package_json(&["next-auth", "next-themes"]),
        );
        assert!(!detected_engineers(nx.path()).contains("nextjs-engineer"));

        let sv = TempDir::new().unwrap();
        write(sv.path(), "package.json", &package_json(&["svelte-check"]));
        assert!(!detected_engineers(sv.path()).contains("svelte-engineer"));
    }

    #[test]
    fn nextjs_detects_by_config_or_dependency() {
        // `next.config.*` is the primary signal, but it is OPTIONAL in Next.js,
        // so the dependency declaration must also work on its own.
        for marker in ["next.config.js", "next.config.mjs", "next.config.ts"] {
            let tmp = TempDir::new().unwrap();
            touch(tmp.path(), marker);
            assert!(
                detected_engineers(tmp.path()).contains("nextjs-engineer"),
                "{marker} must select nextjs-engineer"
            );
        }
        let dep_only = TempDir::new().unwrap();
        write(
            dep_only.path(),
            "package.json",
            &package_json(&["next", "react"]),
        );
        assert!(detected_engineers(dep_only.path()).contains("nextjs-engineer"));
    }

    #[test]
    fn svelte_detects_by_config_or_dependency() {
        for marker in ["svelte.config.js", "svelte.config.ts"] {
            let tmp = TempDir::new().unwrap();
            touch(tmp.path(), marker);
            assert!(
                detected_engineers(tmp.path()).contains("svelte-engineer"),
                "{marker} must select svelte-engineer"
            );
        }
        let dep_only = TempDir::new().unwrap();
        write(dep_only.path(), "package.json", &package_json(&["svelte"]));
        assert!(detected_engineers(dep_only.path()).contains("svelte-engineer"));
    }

    #[test]
    fn plain_elixir_detects_elixir_engineer_only() {
        // #4760: `mix.exs` alone is the ELIXIR marker. Before this change it
        // selected `phoenix-engineer`, giving a plain Elixir project a Phoenix
        // specialist and no general Elixir engineer at all.
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "mix.exs",
            "defmodule My.MixProject do\n  defp deps, do: [{:jason, \"~> 1.4\"}]\nend\n",
        );
        let detected = detected_engineers(tmp.path());
        assert!(detected.contains("elixir-engineer"));
        assert!(
            !detected.contains("phoenix-engineer"),
            "a non-Phoenix Elixir project must not get the Phoenix specialist"
        );
    }

    #[test]
    fn phoenix_dependency_detects_both_elixir_and_phoenix() {
        // A Phoenix app is an Elixir app, so it gets both. `{:phoenix,` must not
        // be satisfied by `{:phoenix_live_view,` alone.
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "mix.exs",
            "defp deps do\n  [{:phoenix, \"~> 1.7.14\"}, {:phoenix_live_view, \"~> 1.0\"}]\nend\n",
        );
        let detected = detected_engineers(tmp.path());
        assert!(detected.contains("phoenix-engineer"));
        assert!(
            detected.contains("elixir-engineer"),
            "Phoenix apps are Elixir apps"
        );

        let live_only = TempDir::new().unwrap();
        write(
            live_only.path(),
            "mix.exs",
            "defp deps do\n  [{:phoenix_live_view, \"~> 1.0\"}]\nend\n",
        );
        assert!(
            !detected_engineers(live_only.path()).contains("phoenix-engineer"),
            "the phoenix_live_view dep alone must not satisfy the phoenix probe"
        );
    }

    #[test]
    fn content_probe_matches_a_declared_dependency() {
        // The probe helper on its own: present-and-matching vs present-and-not.
        let tmp = TempDir::new().unwrap();
        write(tmp.path(), "mix.exs", "[{:phoenix, \"~> 1.7\"}]");
        let budget = ProbeBudget::new();
        assert!(file_contains(
            &tmp.path().join("mix.exs"),
            "{:phoenix,",
            &budget
        ));
        assert!(!file_contains(
            &tmp.path().join("mix.exs"),
            "{:ecto,",
            &budget
        ));
        assert!(
            !file_contains(&tmp.path().join("absent.exs"), "{:phoenix,", &budget),
            "a missing file is an absent marker, never an error"
        );
        assert!(
            !file_contains(tmp.path(), "{:phoenix,", &budget),
            "a directory is not a readable marker file"
        );
    }

    #[test]
    fn content_probe_ignores_an_oversized_file() {
        // The size guard must win even when the needle IS present, so a
        // pathological manifest cannot be read into memory.
        let tmp = TempDir::new().unwrap();
        let mut body = String::from("{:phoenix, \"~> 1.7\"}");
        body.push_str(&"x".repeat(super::super::workspace::MAX_MANIFEST_BYTES as usize + 1));
        write(tmp.path(), "mix.exs", &body);
        let budget = ProbeBudget::new();
        assert!(!file_contains(
            &tmp.path().join("mix.exs"),
            "{:phoenix,",
            &budget
        ));
    }

    #[test]
    fn npm_monorepo_member_declares_react() {
        // #4760 monorepo regression: a turborepo/pnpm root declares no
        // framework dependency, so root-only probing lost react/next/svelte on
        // a Next.js monorepo. The member declaration must be found.
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "package.json",
            r#"{"name":"root","workspaces":["apps/*","packages/*"]}"#,
        );
        write(
            tmp.path(),
            "apps/web/package.json",
            r#"{"dependencies":{"react":"^18","next":"^15"}}"#,
        );
        touch(tmp.path(), "apps/web/next.config.js");

        let detected = detected_engineers(tmp.path());
        assert!(detected.contains("react-engineer"), "{detected:?}");
        assert!(detected.contains("nextjs-engineer"), "{detected:?}");
        assert!(
            detected.contains("javascript-engineer"),
            "the root package.json still selects the language engineers"
        );
    }

    #[test]
    fn monorepo_without_the_framework_still_misses_it() {
        // The walk must not make every monorepo match everything: a workspace
        // whose members declare no framework gets no framework engineer.
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "package.json",
            r#"{"workspaces":["packages/*"]}"#,
        );
        write(
            tmp.path(),
            "packages/util/package.json",
            r#"{"dependencies":{"lodash":"^4"}}"#,
        );
        let detected = detected_engineers(tmp.path());
        assert!(detected.contains("javascript-engineer"));
        for framework in ["react-engineer", "nextjs-engineer", "svelte-engineer"] {
            assert!(!detected.contains(framework), "{framework} must not fire");
        }
    }

    #[test]
    fn elixir_umbrella_member_declares_phoenix() {
        // An umbrella root's mix.exs declares no deps of its own.
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "mix.exs",
            "def project do\n  [apps_path: \"apps\"]\nend\n",
        );
        write(
            tmp.path(),
            "apps/my_app_web/mix.exs",
            "defp deps do\n  [{:phoenix, \"~> 1.7\"}]\nend\n",
        );
        let detected = detected_engineers(tmp.path());
        assert!(detected.contains("elixir-engineer"), "root mix.exs");
        assert!(
            detected.contains("phoenix-engineer"),
            "the umbrella member's phoenix dep must be found: {detected:?}"
        );
    }

    #[test]
    fn monorepo_member_platform_marker_is_found() {
        // Platform detection uses the same probe roots — a Vercel monorepo keeps
        // vercel.json in the deployed app, not at the root.
        let tmp = TempDir::new().unwrap();
        write(tmp.path(), "package.json", r#"{"workspaces":["apps/*"]}"#);
        write(tmp.path(), "apps/web/vercel.json", "{}");
        let detected = detected_platforms(tmp.path());
        assert!(detected.contains("vercel-ops"), "{detected:?}");
        assert!(!detected.contains("gcp-ops"));
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
