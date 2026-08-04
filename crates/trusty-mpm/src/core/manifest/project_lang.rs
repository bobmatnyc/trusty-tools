//! Per-project MARKER EVALUATION for the framework manifest's gated categories
//! (#1941 / HR-2, #4760, #4765).
//!
//! Why: deploying the FULL polyglot agent roster (every `*-engineer`) plus every
//! platform-ops agent to every project regardless of stack is pure noise in the
//! delegation surface and a contributor to the catalog-drift false positives
//! (#1940). This module answers one question — *are this entry's declared
//! markers present in this project?* — for the gated categories
//! `framework-manifest.toml` declares.
//!
//! **What this module no longer owns (#4765).** It used to carry the marker
//! TABLES themselves (`LANGUAGE_ENGINEERS`, `PLATFORM_AGENTS`), which made the
//! gate a Rust constant while the category that used it was a manifest
//! declaration — two authorities for one entry, and two places to drift. The
//! owner ruling of 2026-08-04 ("authoritative agent/skill bundling should be in
//! framework-manifest.toml or project manifest.toml") moved the tables into the
//! manifest as [`super::schema::GatedAgent::markers`]. What is left here is the
//! MECHANISM: how a marker string is evaluated against a directory, bounded and
//! fail-closed. Declaration lives in the manifest; evaluation lives here.
//! What: [`MarkerProbe`] resolves a project's probe roots (the project dir plus
//! every declared workspace member) and a single shared [`ProbeBudget`] once,
//! then answers [`MarkerProbe::detect`] for any list of declared entries. A
//! possibly-EMPTY result is a valid answer, never an error — `super::framework`
//! composes it with the declared categories into the final
//! [`super::schema::AgentSet`]; this module never decides selection policy.
//! Test: `rust_workspace_detects_only_rust_engineer`, `js_project_detects_js_family`,
//! `unknown_project_detects_nothing`, `vercel_marker_detects_vercel_ops`,
//! `gcp_marker_detects_gcp_ops`, `no_platform_marker_detects_nothing`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::schema::GatedAgent;
use super::workspace::{ProbeBudget, probe_roots};

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

/// A project's resolved probe roots and shared read budget (#4765).
///
/// Why: detection asks the same two questions of the same project — which
/// stacks, which platforms — and both must share ONE budget. Two independent
/// `ProbeBudget::new()` calls (what the two former free functions did) let a
/// pathological monorepo spend the aggregate cap twice. Resolving the roots
/// once also avoids re-reading the root manifest's workspace declaration per
/// category.
/// What: holds the probe roots from [`probe_roots`] (project dir first, then
/// every declared workspace member) and one [`ProbeBudget`] for the whole
/// detection call. Construct once per project, then call [`Self::detect`] for
/// each declared category.
/// Test: `budget_is_shared_across_members`, `npm_monorepo_member_declares_react`.
pub(crate) struct MarkerProbe {
    /// The project dir followed by every declared workspace member.
    roots: Vec<PathBuf>,
    /// One aggregate read budget for the whole detection call.
    budget: ProbeBudget,
}

impl MarkerProbe {
    /// Resolve `project_dir`'s probe roots under a fresh shared budget.
    pub(crate) fn new(project_dir: &Path) -> Self {
        let budget = ProbeBudget::new();
        let roots = probe_roots(project_dir, &budget);
        Self { roots, budget }
    }

    /// Whether any of `entry`'s declared markers is present at any probe root.
    ///
    /// Why: a declared entry deploys on ANY of its markers, not all — a Java
    /// project is Maven OR Gradle, never required to be both.
    /// What: the OR over [`marker_present_in_any`] for `entry.markers`. An entry
    /// with no markers can never match; `parse_framework_manifest` rejects that
    /// case up front so it is unreachable from the bundled manifest.
    /// Test: `java_project_detects_java_engineer`,
    /// `content_probe_does_not_match_a_longer_named_sibling`.
    fn selects(&self, entry: &GatedAgent) -> bool {
        entry
            .markers
            .iter()
            .any(|marker| marker_present_in_any(&self.roots, marker, &self.budget))
    }

    /// The stems of `entries` whose declared markers this project shows.
    ///
    /// Why: the one detection primitive `super::framework::agent_scope_from`
    /// calls, once per gated category.
    /// What: a sorted, de-duplicated set of matching stems. An EMPTY result is a
    /// valid, expected answer meaning "this project shows none of these markers"
    /// — never confused with a manifest error, which is a distinct `Err` on a
    /// different code path (`super::framework::parse_framework_manifest`).
    /// Test: `rust_workspace_detects_only_rust_engineer`,
    /// `no_platform_marker_detects_nothing`.
    pub(crate) fn detect(&self, entries: &[GatedAgent]) -> BTreeSet<String> {
        entries
            .iter()
            .filter(|entry| self.selects(entry))
            .map(|entry| entry.stem.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::manifest::framework::framework_agent_categories;
    use std::fs;
    use tempfile::TempDir;

    /// The stack (`language` + `framework`) stems the BUNDLED manifest's markers
    /// select for `dir`.
    ///
    /// Why: every detection test below asserts against the real shipped
    /// declaration, not a fixture — that is what makes them a check on
    /// `framework-manifest.toml`'s markers rather than on a second copy of them.
    /// Test: this helper IS the test surface for the tests that call it.
    fn detected_engineers(dir: &Path) -> BTreeSet<String> {
        let categories = framework_agent_categories().expect("bundled manifest must be valid");
        let probe = MarkerProbe::new(dir);
        let mut detected = probe.detect(&categories.language);
        detected.extend(probe.detect(&categories.framework));
        detected
    }

    /// The `platform` stems the BUNDLED manifest's markers select for `dir`.
    fn detected_platforms(dir: &Path) -> BTreeSet<String> {
        let categories = framework_agent_categories().expect("bundled manifest must be valid");
        MarkerProbe::new(dir).detect(&categories.platform)
    }

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
    fn js_project_detects_js_family() {
        // A JS/TS project detects the whole LANGUAGE family — `package.json`
        // selects javascript-engineer and (with tsconfig.json, or on its own)
        // typescript-engineer. The framework engineers are a separate question,
        // covered by `bare_js_project_detects_no_framework_engineer`.
        let tmp = TempDir::new().unwrap();
        write(tmp.path(), "package.json", &package_json(&["lodash"]));
        touch(tmp.path(), "tsconfig.json");

        let detected = detected_engineers(tmp.path());
        assert!(detected.contains("javascript-engineer"));
        assert!(detected.contains("typescript-engineer"));
        assert!(
            !detected.contains("rust-engineer"),
            "a JS project must not detect a foreign language engineer"
        );
    }

    #[test]
    fn java_project_detects_java_engineer() {
        // `java-engineer` declares FOUR markers; any ONE selects it. Maven and
        // Gradle are checked independently so a regression that silently
        // required all of them (an AND where the manifest means OR) is caught.
        for marker in ["pom.xml", "build.gradle", "build.gradle.kts"] {
            let tmp = TempDir::new().unwrap();
            touch(tmp.path(), marker);
            assert!(
                detected_engineers(tmp.path()).contains("java-engineer"),
                "`{marker}` alone must select java-engineer"
            );
        }
    }

    #[test]
    fn dotnet_csproj_detects_dotnet_engineer() {
        // The `*.<ext>` GLOB branch of `marker_present`, which no fixed-name
        // marker exercises: .NET project files are named after the project, so
        // the manifest declares `*.csproj` / `*.sln` / `*.vbproj`.
        for marker in ["MyApp.csproj", "MyApp.sln", "Legacy.vbproj"] {
            let tmp = TempDir::new().unwrap();
            touch(tmp.path(), marker);
            assert!(
                detected_engineers(tmp.path()).contains("dotnet-engineer"),
                "`{marker}` must match the extension glob and select dotnet-engineer"
            );
        }

        // The glob must not fire on a same-stem file with a different extension.
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "MyApp.csproj.bak");
        assert!(
            !detected_engineers(tmp.path()).contains("dotnet-engineer"),
            "`.csproj.bak` does not end in `.csproj` and must not match"
        );
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
    fn an_entry_with_no_markers_never_selects() {
        // #4765: markers are a manifest field now, so "declared but ungated" is
        // representable in the type. It must select nothing — and
        // `parse_framework_manifest` rejects it, so it can never ship.
        let tmp = TempDir::new().unwrap();
        touch(tmp.path(), "Cargo.toml");
        let ungated = GatedAgent {
            stem: "ungated-engineer".to_string(),
            markers: Vec::new(),
        };
        assert!(
            MarkerProbe::new(tmp.path()).detect(&[ungated]).is_empty(),
            "an entry declaring no markers can never be selected"
        );
    }
}
