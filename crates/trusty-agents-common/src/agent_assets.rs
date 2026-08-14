//! The agent-asset roster: one physical `.md` per agent, shared by every consumer.
//!
//! Why: `trusty-mpm` and `trusty-code` each shipped a byte-identical copy of 30
//! agent `.md` files, kept in step only by a CI diff
//! (`scripts/check_agent_assets.sh`) that could fail long after an edit landed.
//! Two copies of one prompt is the defect: a fix to an agent's instructions had
//! to be applied twice, and any miss surfaced as a red gate on `main` rather
//! than at the edit. Per the owner's one-copy ruling the roster lives here, in
//! the crate both consumers already depend on, and each consumer embeds THIS
//! file. Drift is no longer detected — it is unrepresentable.
//!
//! All 42 moved, not just the 30 that were duplicated. `agents::builder`
//! resolves an `extends:` reference within a SINGLE directory, so relocating
//! the 5 `BASE-*` templates while leaving the 12 trusty-mpm-only agents behind
//! would have broken their extends targets by construction. Keeping the roster
//! whole makes this directory a self-contained composable unit, which is what
//! every consumer's compose step (and `trusty-mpm`'s asset tests) needs.
//!
//! What: 42 `pub const &str` items, each an `include_str!` of the matching file
//! under `assets/agents/`, plus [`AGENT_ASSETS`] pairing every original
//! filename with its content.
//! Test: `agent_assets::tests` — every const non-empty, filenames unique, each
//! row embedding the file it names, no two rows sharing a string, the `BASE-*`
//! templates present, and the table an exact match for the directory listing.
//!
//! `trusty-code` embeds 30 of these (the coding-relevant roster #2958 selected)
//! and keeps 8 files of its own: 4 deliberate forks that add a read-only
//! `tools:` restriction, and 4 defaults with no counterpart here. Those are
//! single copies already, so the one-copy rule has nothing to say about them.

/// Root of every trusty-mpm inheritance chain.
pub const BASE_AGENT: &str = include_str!("assets/agents/BASE-AGENT.md");

/// Foundation for all engineer agents.
pub const BASE_ENGINEER: &str = include_str!("assets/agents/BASE-ENGINEER.md");

/// Foundation for all ops agents.
pub const BASE_OPS: &str = include_str!("assets/agents/BASE-OPS.md");

/// Foundation for all QA agents.
pub const BASE_QA: &str = include_str!("assets/agents/BASE-QA.md");

/// Foundation for all research agents.
pub const BASE_RESEARCH: &str = include_str!("assets/agents/BASE-RESEARCH.md");

/// API/backend testing specialist (`extends: base-qa`).
pub const API_QA: &str = include_str!("assets/agents/api-qa.md");

/// Static-analysis and code-health analyst (`extends: base-research`).
pub const CODE_ANALYZER: &str = include_str!("assets/agents/code-analyzer.md");

/// Adversarial reviewer (`extends: base-qa`).
pub const CODE_CRITIC: &str = include_str!("assets/agents/code-critic.md");

/// Dart/Flutter engineer (`extends: base-engineer`).
pub const DART_ENGINEER: &str = include_str!("assets/agents/dart-engineer.md");

/// ETL / data-transformation engineer (`extends: base-engineer`).
pub const DATA_ENGINEER: &str = include_str!("assets/agents/data-engineer.md");

/// Technical-documentation specialist (`extends: base-agent`).
pub const DOCUMENTATION: &str = include_str!("assets/agents/documentation.md");

/// C#/.NET engineer with VB.NET awareness (`extends: base-engineer`).
pub const DOTNET_ENGINEER: &str = include_str!("assets/agents/dotnet-engineer.md");

/// Elixir/OTP engineer (`extends: base-engineer`).
pub const ELIXIR_ENGINEER: &str = include_str!("assets/agents/elixir-engineer.md");

/// General-purpose implementation engineer (`extends: base-engineer`).
///
/// Distinct from `trusty-code`'s own `engineer` default, which deliberately
/// does not track this one — that name collision is why #2958 left this agent
/// out of the tcode roster.
pub const ENGINEER: &str = include_str!("assets/agents/engineer.md");

/// Google Cloud Platform operations (`extends: base-ops`).
pub const GCP_OPS: &str = include_str!("assets/agents/gcp-ops.md");

/// Go engineer (`extends: base-engineer`).
pub const GOLANG_ENGINEER: &str = include_str!("assets/agents/golang-engineer.md");

/// Java engineer (`extends: base-engineer`).
pub const JAVA_ENGINEER: &str = include_str!("assets/agents/java-engineer.md");

/// Vanilla-JavaScript engineer (`extends: base-engineer`).
pub const JAVASCRIPT_ENGINEER: &str = include_str!("assets/agents/javascript-engineer.md");

/// Local dev-environment operations (`extends: base-ops`).
pub const LOCAL_OPS: &str = include_str!("assets/agents/local-ops.md");

/// trusty-memory MCP memory curator (`extends: base-agent`).
pub const MEMORY_MANAGER: &str = include_str!("assets/agents/memory-manager.md");

/// Bundled-asset catalog lifecycle (`extends: base-agent`).
pub const MPM_AGENT_MANAGER: &str = include_str!("assets/agents/mpm-agent-manager.md");

/// Skill lifecycle and recommendations (`extends: base-agent`).
pub const MPM_SKILLS_MANAGER: &str = include_str!("assets/agents/mpm-skills-manager.md");

/// Next.js engineer (`extends: base-engineer`).
pub const NEXTJS_ENGINEER: &str = include_str!("assets/agents/nextjs-engineer.md");

/// Phoenix web-layer engineer (`extends: base-engineer`).
pub const PHOENIX_ENGINEER: &str = include_str!("assets/agents/phoenix-engineer.md");

/// PHP/Laravel engineer (`extends: base-engineer`).
pub const PHP_ENGINEER: &str = include_str!("assets/agents/php-engineer.md");

/// Prompt/LLM-optimization engineer (`extends: base-engineer`).
pub const PROMPT_ENGINEER: &str = include_str!("assets/agents/prompt-engineer.md");

/// Python engineer (`extends: base-engineer`).
pub const PYTHON_ENGINEER: &str = include_str!("assets/agents/python-engineer.md");

/// Quality-assurance engineer (`extends: base-qa`).
pub const QA: &str = include_str!("assets/agents/qa.md");

/// React engineer (`extends: base-engineer`).
pub const REACT_ENGINEER: &str = include_str!("assets/agents/react-engineer.md");

/// Behavior-preserving refactoring engineer (`extends: base-engineer`).
pub const REFACTORING_ENGINEER: &str = include_str!("assets/agents/refactoring-engineer.md");

/// Codebase/architecture research analyst (`extends: base-research`).
pub const RESEARCH: &str = include_str!("assets/agents/research.md");

/// Ruby/Rails engineer (`extends: base-engineer`).
pub const RUBY_ENGINEER: &str = include_str!("assets/agents/ruby-engineer.md");

/// Rust engineer (`extends: base-engineer`).
pub const RUST_ENGINEER: &str = include_str!("assets/agents/rust-engineer.md");

/// Security / vulnerability-assessment specialist (`extends: base-agent`).
pub const SECURITY: &str = include_str!("assets/agents/security.md");

/// Svelte 5 / SvelteKit engineer (`extends: base-engineer`).
pub const SVELTE_ENGINEER: &str = include_str!("assets/agents/svelte-engineer.md");

/// Tauri desktop-application engineer (`extends: base-engineer`).
pub const TAURI_ENGINEER: &str = include_str!("assets/agents/tauri-engineer.md");

/// Issue/ticket lifecycle specialist (`extends: base-agent`).
pub const TICKETING: &str = include_str!("assets/agents/ticketing.md");

/// TypeScript engineer (`extends: base-engineer`).
pub const TYPESCRIPT_ENGINEER: &str = include_str!("assets/agents/typescript-engineer.md");

/// Vercel platform operations (`extends: base-ops`).
pub const VERCEL_OPS: &str = include_str!("assets/agents/vercel-ops.md");

/// Git/branch/release operations (`extends: base-ops`).
pub const VERSION_CONTROL: &str = include_str!("assets/agents/version-control.md");

/// Browser-driven web QA (`extends: base-qa`).
pub const WEB_QA: &str = include_str!("assets/agents/web-qa.md");

/// Front-end web/UI engineer (`extends: base-engineer`).
pub const WEB_UI_ENGINEER: &str = include_str!("assets/agents/web-ui-engineer.md");

/// Every agent asset as `(original_filename, content)`, `BASE-*` first then
/// alphabetical.
///
/// Why: `trusty-code` composes `extends:` chains in memory from a batch table
/// keyed by filename (`BASE-QA.md` resolves an `extends: base-qa` reference
/// after that map's own case-folding), so it needs the filenames, not just the
/// named items. The table also carries the completeness guarantee: a `.md`
/// added to `assets/agents/` but never wired up fails
/// `table_matches_the_directory` rather than shipping as a file nothing embeds.
/// What: 42 pairs, each value the same `&'static str` as the matching named
/// const above.
/// Test: `agent_assets::tests::every_entry_embeds_the_file_it_names`,
/// `agent_assets::tests::table_matches_the_directory`.
pub const AGENT_ASSETS: &[(&str, &str)] = &[
    ("BASE-AGENT.md", BASE_AGENT),
    ("BASE-ENGINEER.md", BASE_ENGINEER),
    ("BASE-OPS.md", BASE_OPS),
    ("BASE-QA.md", BASE_QA),
    ("BASE-RESEARCH.md", BASE_RESEARCH),
    ("api-qa.md", API_QA),
    ("code-analyzer.md", CODE_ANALYZER),
    ("code-critic.md", CODE_CRITIC),
    ("dart-engineer.md", DART_ENGINEER),
    ("data-engineer.md", DATA_ENGINEER),
    ("documentation.md", DOCUMENTATION),
    ("dotnet-engineer.md", DOTNET_ENGINEER),
    ("elixir-engineer.md", ELIXIR_ENGINEER),
    ("engineer.md", ENGINEER),
    ("gcp-ops.md", GCP_OPS),
    ("golang-engineer.md", GOLANG_ENGINEER),
    ("java-engineer.md", JAVA_ENGINEER),
    ("javascript-engineer.md", JAVASCRIPT_ENGINEER),
    ("local-ops.md", LOCAL_OPS),
    ("memory-manager.md", MEMORY_MANAGER),
    ("mpm-agent-manager.md", MPM_AGENT_MANAGER),
    ("mpm-skills-manager.md", MPM_SKILLS_MANAGER),
    ("nextjs-engineer.md", NEXTJS_ENGINEER),
    ("phoenix-engineer.md", PHOENIX_ENGINEER),
    ("php-engineer.md", PHP_ENGINEER),
    ("prompt-engineer.md", PROMPT_ENGINEER),
    ("python-engineer.md", PYTHON_ENGINEER),
    ("qa.md", QA),
    ("react-engineer.md", REACT_ENGINEER),
    ("refactoring-engineer.md", REFACTORING_ENGINEER),
    ("research.md", RESEARCH),
    ("ruby-engineer.md", RUBY_ENGINEER),
    ("rust-engineer.md", RUST_ENGINEER),
    ("security.md", SECURITY),
    ("svelte-engineer.md", SVELTE_ENGINEER),
    ("tauri-engineer.md", TAURI_ENGINEER),
    ("ticketing.md", TICKETING),
    ("typescript-engineer.md", TYPESCRIPT_ENGINEER),
    ("vercel-ops.md", VERCEL_OPS),
    ("version-control.md", VERSION_CONTROL),
    ("web-qa.md", WEB_QA),
    ("web-ui-engineer.md", WEB_UI_ENGINEER),
];

/// Absolute path to the embedded roster's source directory.
///
/// Why: `trusty-mpm`'s asset tests compose the real bundled agents through
/// `agents::builder::compose_agent`, which reads from a directory rather than
/// from embedded strings. Those tests used to point at trusty-mpm's own
/// `src/assets/agents`; now that the roster lives here, they need this path,
/// and hard-coding `../trusty-agents-common/...` in a sibling crate would break
/// the moment either crate moved.
/// What: `CARGO_MANIFEST_DIR` of THIS crate joined with `src/assets/agents`,
/// resolved at compile time.
/// Test: `agent_assets::tests::table_matches_the_directory` reads it.
pub const AGENT_ASSETS_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/assets/agents");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_asset_is_non_empty() {
        for (name, body) in AGENT_ASSETS {
            assert!(
                !body.trim().is_empty(),
                "agent asset `{name}` embedded empty"
            );
        }
    }

    #[test]
    fn table_filenames_are_unique() {
        let mut seen: Vec<&str> = AGENT_ASSETS.iter().map(|(n, _)| *n).collect();
        let total = seen.len();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(total, seen.len(), "duplicate filename in AGENT_ASSETS");
        assert_eq!(total, 42, "AGENT_ASSETS must carry all 42 agent assets");
    }

    /// Each row's content must be the file its filename names. Catches the
    /// copy-paste that wires `("qa.md", WEB_QA)` — a mis-mapping no count or
    /// uniqueness check can see, and one that would silently ship the wrong
    /// prompt under the right name.
    #[test]
    fn every_entry_embeds_the_file_it_names() {
        for (name, body) in AGENT_ASSETS {
            let path = std::path::Path::new(AGENT_ASSETS_DIR).join(name);
            let on_disk = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
            assert_eq!(
                on_disk, *body,
                "`{name}`'s table entry does not embed {name} — check the \
                 const wired to that filename"
            );
        }
    }

    /// No two filenames may embed the same string. A duplicate pointer means
    /// one agent's row was pasted over another's, which
    /// `every_entry_embeds_the_file_it_names` would also catch — this names the
    /// failure directly instead of reporting a content mismatch.
    #[test]
    fn no_two_entries_share_content() {
        for (i, (name_a, body_a)) in AGENT_ASSETS.iter().enumerate() {
            for (name_b, body_b) in AGENT_ASSETS.iter().skip(i + 1) {
                assert!(
                    !std::ptr::eq(*body_a, *body_b),
                    "`{name_a}` and `{name_b}` embed the same string"
                );
            }
        }
    }

    /// The completeness half of the guarantee `scripts/check_agent_assets.sh`
    /// used to provide: byte-parity between two copies is gone because there is
    /// only one copy, but "a file exists that nothing embeds" is still possible
    /// and this is what catches it.
    #[test]
    fn table_matches_the_directory() {
        let mut on_disk: Vec<String> = std::fs::read_dir(AGENT_ASSETS_DIR)
            .expect("the embedded agent roster directory must exist")
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".md"))
            .collect();
        on_disk.sort();

        let mut in_table: Vec<String> =
            AGENT_ASSETS.iter().map(|(n, _)| (*n).to_string()).collect();
        in_table.sort();

        assert_eq!(
            on_disk, in_table,
            "AGENT_ASSETS and {AGENT_ASSETS_DIR} disagree — a roster file was \
             added or removed without updating the table, so it would ship \
             embedded by nobody (or be embedded while missing from disk)"
        );
    }

    /// A blocked agent must not reach for a more-privileged `gh` credential.
    /// See #5680 — a `version-control` agent hit a `BEHIND` branch-protection
    /// block, borrowed the repo owner's token, and force-merged with `--admin`.
    /// The prohibition ships in two places on purpose: `BASE-AGENT.md` binds
    /// every composed agent, and `version-control.md` puts it beside the
    /// `gh auth status` check the acting agent actually read.
    #[test]
    fn credential_switching_is_forbidden_in_the_shipped_assets() {
        for (name, body) in [
            ("BASE-AGENT.md", BASE_AGENT),
            ("version-control.md", VERSION_CONTROL),
        ] {
            let flat = body.replace('\n', " ");
            assert!(
                flat.contains("Never switch") && flat.contains("credential"),
                "`{name}` must forbid switching to another `gh` \
                 account/token/credential to obtain a missing permission (#5680)"
            );
        }

        // #2842 went the other way — the agent refused an authorized
        // admin-merge — so the ban above must not swallow that path.
        assert!(
            VERSION_CONTROL.contains("When the PM relays operator authorization to merge directly"),
            "the credential-switching ban must leave the PM-relayed \
             admin-merge authorization intact (#2842)"
        );
    }

    /// The `BASE-*` templates are what every other asset's `extends:` chain
    /// roots at. Shipping the roster without them would make composition
    /// impossible for every consumer, which is precisely why all 42 moved
    /// together rather than only the 30 that were duplicated.
    #[test]
    fn base_templates_travel_with_the_roster() {
        for base in [
            "BASE-AGENT.md",
            "BASE-ENGINEER.md",
            "BASE-OPS.md",
            "BASE-QA.md",
            "BASE-RESEARCH.md",
        ] {
            assert!(
                AGENT_ASSETS.iter().any(|(n, _)| *n == base),
                "`{base}` must ship alongside the agents that extend it"
            );
        }
    }
}
