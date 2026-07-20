//! Progressive-disclosure skill discovery for `.claude/skills/` (#2069, vision
//! spec §5.3, Resolved Decision #7).
//!
//! Why: A project's `.claude/skills/` catalog can hold dozens of skills, but
//! any given task only ever invokes a handful of them. Injecting every
//! skill's full Markdown body into the system prompt up front wastes
//! 500-1000+ tokens per session for a 20-skill catalog (spec §5.3 impact
//! estimate) when ~80% of skills are never invoked. Splitting a skill into
//! cheap, always-cached metadata (name + one-line description, ~100 tokens
//! per skill) and an on-demand-loaded body lets the PM/agents see the full
//! catalog for free and pay the body's token cost only for skills they
//! actually use. This module mirrors `agents::discover_agents`'s directory
//! scanning pattern, adapted for the Claude-Code skill layout: one
//! subdirectory per skill, each holding a `SKILL.md` with YAML-ish
//! frontmatter (`name:`, `description:`, …) followed by the Markdown body.
//! What: [`SkillMetadata`] (the cached catalog entry), [`locate_skills_dir`]
//! (resolves `<project_root>/.claude/skills` — the Claude-Code-compatible
//! path; deliberately NOT `~/.trusty-mpm/skills/`), [`discover_skill_metadata`]
//! (scans the directory and parses just the frontmatter of every skill),
//! [`load_skill_body`] (the lazy, on-invoke full-body loader), and
//! [`format_skill_catalog`] (renders the metadata list for prompt injection).
//! [`FsSkillResolver`] implements `tools::traits::SkillResolver` over this
//! discovery layer, giving the tool-registry layer a ready-to-register
//! concrete resolver.
//! `discover_skill_metadata` falls back to `crate::assets::DEFAULT_SKILLS`
//! (#2895) when the disk directory has no skills — a project with no
//! `.claude/skills/` (or an empty one) still gets trusty-mpm's universal
//! skill catalog. A disk directory with even one skill is used as-is (never
//! merged with the embedded set) — disk always wins.
//! Test: `skills::tests::*` — discovery over tempdir fixtures (happy path,
//! missing dir, missing `SKILL.md`, malformed/no-frontmatter file), lazy body
//! load (found + unknown-name), catalog formatting (empty + populated),
//! embedded-fallback threshold, and `FsSkillResolver`'s `SkillResolver` trait
//! conformance.

mod frontmatter;
pub mod protocol;

use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::tools::traits::SkillResolver;
use frontmatter::{parse_frontmatter, strip_frontmatter};

/// Cheap, always-cached catalog entry for one discovered skill.
///
/// Why: The PM/agents need to see what skills exist and when to reach for
/// them without paying for every skill's full body up front.
/// What: `name` (frontmatter `name:`, falling back to the skill's directory
/// name) and `description` (frontmatter `description:`, empty string if
/// absent).
/// Test: `discover_skill_metadata_reads_name_and_description`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillMetadata {
    pub name: String,
    pub description: String,
}

/// Errors from lazily loading a skill's full body.
///
/// Why: A named error type lets `SkillResolver::resolve` and the `use_skill`
/// tool distinguish "unknown skill" (an LLM hallucinated a name) from a
/// genuine IO failure, both without panicking.
/// What: `Unknown` — the name is not in the discovered catalog; `Io` — the
/// `SKILL.md` file could not be read even though the name is known.
/// Test: `load_skill_body_rejects_unknown_name`, `load_skill_body_io_error`.
#[derive(Debug, Error)]
pub enum SkillError {
    #[error("skill '{0}' is not in the discovered catalog")]
    Unknown(String),
    #[error("failed to read skill body for '{name}': {source}")]
    Io {
        name: String,
        #[source]
        source: std::io::Error,
    },
}

/// Locate the Claude-Code-compatible skills directory for a project root.
///
/// Why: #2069 pins this to `.claude/skills/` exactly — the Claude-Code-native
/// convention — and explicitly NOT the legacy `~/.trusty-mpm/skills/` path
/// trusty-mpm uses, so a project's skill catalog resolves identically whether
/// browsed by a human in Claude Code or discovered by `tcode`.
/// What: Returns `<project_root>/.claude/skills` (may not exist yet — callers
/// handle absence via `discover_skill_metadata`'s empty-Vec fallback).
/// Test: `locate_skills_dir_is_dot_claude_skills`.
pub fn locate_skills_dir(project_root: &Path) -> PathBuf {
    project_root.join(".claude").join("skills")
}

/// Discover skill metadata from every `<dir>/<skill-name>/SKILL.md`.
///
/// Why: The catalog must be built once, cheaply, from cheap-to-read
/// frontmatter only — the whole point of progressive disclosure is to avoid
/// reading full skill bodies until they are actually invoked. A project that
/// has not yet created `.claude/skills/` (or has created it but left it
/// empty) must not start with zero skills — see the embedded-fallback note
/// below.
/// What: Scans immediate subdirectories of `dir`; a subdirectory without a
/// readable `SKILL.md` is skipped (not an error — mirrors
/// `agents::discover_agents`'s graceful-skip convention). Returns entries
/// sorted by `name`. If the scan yields zero entries (missing/unreadable
/// `dir`, or an existing-but-empty one — empty is the fallback threshold; a
/// disk directory with even one discoverable skill is used as-is, never
/// merged with the embedded set), falls back to `embedded_skill_metadata`
/// (`crate::assets::DEFAULT_SKILLS`, #2895). Disk skills always win when
/// present.
/// Test: `discover_skill_metadata_reads_name_and_description`,
/// `discover_skill_metadata_skips_dir_without_skill_md`,
/// `discover_skill_metadata_falls_back_to_embedded_when_disk_empty`,
/// `discover_skill_metadata_disk_wins_when_present`,
/// `discover_skill_metadata_falls_back_to_dirname`.
pub fn discover_skill_metadata(dir: &Path) -> Vec<SkillMetadata> {
    let skills = discover_disk_skill_metadata(dir);
    if skills.is_empty() {
        return embedded_skill_metadata();
    }
    skills
}

/// Scan `dir` for disk skills WITHOUT the embedded-fallback threshold —
/// returns an empty `Vec` for a missing/empty directory rather than
/// substituting `embedded_skill_metadata()` (#3449).
///
/// Why: `protocol::skills_list` needs to distinguish "this project genuinely
/// has zero custom skills" from "here is the bundled catalog" — merging the
/// two (as [`discover_skill_metadata`]'s fallback does, correctly, for the
/// *tool-facing* discovery/prompt-injection path) would mislabel every
/// bundled entry as `tier: "project"` in the catalog endpoint whenever a
/// project's `.claude/skills/` happens to be empty. Factored out of
/// [`discover_skill_metadata`] so the fallback threshold lives in exactly
/// one place; the tool-facing function above is unchanged in behavior.
/// What: identical directory scan/sort to [`discover_skill_metadata`], minus
/// the embedded-fallback branch.
/// Test: `protocol::tests::list_returns_disk_only_when_disk_non_empty_no_bundled_entries`,
/// `protocol::tests::list_returns_bundled_when_projectless`,
/// `protocol::tests::list_returns_bundled_when_disk_empty`.
pub(crate) fn discover_disk_skill_metadata(dir: &Path) -> Vec<SkillMetadata> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        tracing::debug!("skills dir not found or unreadable: {}", dir.display());
        return Vec::new();
    };

    let mut skills: Vec<SkillMetadata> = entries
        .flatten()
        .filter_map(|entry| load_metadata_from_skill_dir(&entry.path()))
        .collect();
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    skills
}

/// Build the embedded-default skill catalog from `crate::assets::DEFAULT_SKILLS`.
///
/// Why: The embedded-fallback path for `discover_skill_metadata` — parses
/// each bundled `SKILL.md`'s frontmatter in-memory, exactly as
/// `load_metadata_from_skill_dir` does for a disk file, so the resulting
/// `SkillMetadata` is indistinguishable from a disk-discovered one.
/// What: Maps every `EmbeddedSkill` through `parse_frontmatter`, sorted by
/// name (the table is already declared in name order, but this does not
/// depend on that).
/// Test: `discover_skill_metadata_falls_back_to_embedded_when_disk_empty`.
fn embedded_skill_metadata() -> Vec<SkillMetadata> {
    let mut skills: Vec<SkillMetadata> = crate::assets::DEFAULT_SKILLS
        .iter()
        .map(|embedded| {
            let front = parse_frontmatter(embedded.skill_md);
            SkillMetadata {
                name: front
                    .get("name")
                    .cloned()
                    .unwrap_or_else(|| embedded.name.to_string()),
                description: front.get("description").cloned().unwrap_or_default(),
            }
        })
        .collect();
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    skills
}

/// Load one skill directory's metadata, if it holds a readable `SKILL.md`.
///
/// Why: Factored out of `discover_skill_metadata` so each rejection reason
/// (not a directory, no `SKILL.md`, unreadable) can be logged distinctly.
/// What: Returns `None` (with a debug log) for anything that is not a skill
/// directory; otherwise parses frontmatter and falls back to the directory
/// name when `name:` is absent.
/// Test: covered via `discover_skill_metadata`'s tests above.
fn load_metadata_from_skill_dir(path: &Path) -> Option<SkillMetadata> {
    if !path.is_dir() {
        return None;
    }
    let dirname = path.file_name()?.to_str()?.to_string();
    let skill_md = path.join("SKILL.md");
    let raw = match std::fs::read_to_string(&skill_md) {
        Ok(raw) => raw,
        Err(e) => {
            tracing::debug!("skipping skill dir '{dirname}': no readable SKILL.md ({e})");
            return None;
        }
    };
    let front = parse_frontmatter(&raw);
    let name = front.get("name").cloned().unwrap_or(dirname);
    let description = front.get("description").cloned().unwrap_or_default();
    Some(SkillMetadata { name, description })
}

/// Lazily load one skill's full Markdown body (the frontmatter stripped off).
///
/// Why: This is the "on invoke" half of progressive disclosure — the body is
/// read from disk only when a caller (the `use_skill` tool, or a resolver's
/// `resolve`) actually needs it.
/// What: Rejects any `name` not present in `known` (the cached catalog) up
/// front — this doubles as the path-traversal guard, since the join'd path is
/// only ever built from a name the catalog itself already discovered (on disk
/// or embedded). Reads `<skills_dir>/<name>/SKILL.md`; if that read fails
/// (the disk fallback case: `known` came from `embedded_skill_metadata`, so
/// there is no such file) and `name` matches an entry in
/// `crate::assets::DEFAULT_SKILLS`, returns that embedded body instead
/// (#2895). Otherwise propagates the disk `Io` error. Either way, the
/// frontmatter fence is stripped and the body trimmed.
/// Test: `load_skill_body_returns_body_without_frontmatter`,
/// `load_skill_body_rejects_unknown_name`,
/// `load_skill_body_falls_back_to_embedded_when_disk_read_fails`.
pub fn load_skill_body(
    skills_dir: &Path,
    name: &str,
    known: &[SkillMetadata],
) -> Result<String, SkillError> {
    if !known.iter().any(|m| m.name == name) {
        return Err(SkillError::Unknown(name.to_string()));
    }
    let path = skills_dir.join(name).join("SKILL.md");
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(source) => {
            if let Some(embedded) = crate::assets::DEFAULT_SKILLS
                .iter()
                .find(|e| e.name == name)
            {
                return Ok(strip_frontmatter(embedded.skill_md).trim().to_string());
            }
            return Err(SkillError::Io {
                name: name.to_string(),
                source,
            });
        }
    };
    Ok(strip_frontmatter(&raw).trim().to_string())
}

/// Render the compact, always-injected skill catalog for prompt assembly.
///
/// Why: This is the entire token-efficiency payoff — a one-line-per-skill
/// listing instead of every skill's full body.
/// What: Returns an empty string when `skills` is empty (callers treat an
/// empty section as "omit it", matching `assemble_system_prompt`'s
/// empty-section-skipping convention). Otherwise a header line plus one
/// `- name: description` line per skill, in the given order (callers pass
/// already name-sorted `Vec`s from `discover_skill_metadata`).
/// Test: `format_skill_catalog_empty_is_empty_string`,
/// `format_skill_catalog_lists_every_skill`.
pub fn format_skill_catalog(skills: &[SkillMetadata]) -> String {
    if skills.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "Available skills (invoke the `use_skill` tool with the skill name to load its full instructions):\n",
    );
    for s in skills {
        out.push_str(&format!("- {}: {}\n", s.name, s.description));
    }
    out.trim_end().to_string()
}

/// Filesystem-backed [`SkillResolver`] over a `.claude/skills/` directory.
///
/// Why: Gives the tool-registry layer (the `use_skill` tool) a ready
/// concrete resolver: metadata is discovered once at construction (cheap,
/// cached); `resolve` reads a single skill's body lazily, on demand.
/// What: Wraps `skills_dir` plus the metadata discovered from it at
/// construction time. `metadata()`/`list()` return the cached catalog;
/// `resolve()` calls `load_skill_body` per invocation.
/// Test: `fs_skill_resolver_resolves_known_skill`,
/// `fs_skill_resolver_list_matches_metadata_names`,
/// `fs_skill_resolver_resolve_unknown_returns_none`.
pub struct FsSkillResolver {
    skills_dir: PathBuf,
    metadata: Vec<SkillMetadata>,
}

impl FsSkillResolver {
    /// Construct a resolver, eagerly discovering (cheap) metadata.
    ///
    /// Why: Metadata discovery is a directory scan plus frontmatter parsing
    /// only — cheap enough to do once at construction so every later
    /// `metadata()`/`list()` call is a clone of an in-memory `Vec`.
    /// What: Calls `discover_skill_metadata(&skills_dir)` and stores both.
    /// Test: `fs_skill_resolver_resolves_known_skill`.
    pub fn new(skills_dir: impl Into<PathBuf>) -> Self {
        let skills_dir = skills_dir.into();
        let metadata = discover_skill_metadata(&skills_dir);
        Self {
            skills_dir,
            metadata,
        }
    }
}

impl SkillResolver for FsSkillResolver {
    fn resolve(&self, name: &str) -> Option<String> {
        load_skill_body(&self.skills_dir, name, &self.metadata).ok()
    }

    fn list(&self) -> Vec<String> {
        self.metadata.iter().map(|m| m.name.clone()).collect()
    }

    fn metadata(&self) -> Vec<SkillMetadata> {
        self.metadata.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_skill(root: &Path, name: &str, frontmatter: &str, body: &str) {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).expect("mkdir skill dir");
        std::fs::write(dir.join("SKILL.md"), format!("{frontmatter}{body}")).expect("write");
    }

    /// `locate_skills_dir` returns `<project_root>/.claude/skills` exactly.
    ///
    /// Why: Pins the Claude-Code-compatible path (#2069) so a future edit
    /// cannot silently drift back to `~/.trusty-mpm/skills/`.
    /// Test: this test.
    #[test]
    fn locate_skills_dir_is_dot_claude_skills() {
        let root = Path::new("/fake/project");
        assert_eq!(locate_skills_dir(root), root.join(".claude").join("skills"));
    }

    /// Discovery reads `name`/`description` from frontmatter for every
    /// skill directory containing a `SKILL.md`.
    ///
    /// Why: This is the core "always cached, cheap" discovery contract.
    /// What: Two skill dirs; asserts both appear, sorted by name, with the
    /// exact frontmatter values.
    /// Test: this test.
    #[test]
    fn discover_skill_metadata_reads_name_and_description() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_skill(
            tmp.path(),
            "zeta-skill",
            "---\nname: zeta-skill\ndescription: Does zeta things\n---\n",
            "# Zeta\nbody\n",
        );
        write_skill(
            tmp.path(),
            "alpha-skill",
            "---\nname: alpha-skill\ndescription: Does alpha things\n---\n",
            "# Alpha\nbody\n",
        );

        let skills = discover_skill_metadata(tmp.path());
        assert_eq!(skills.len(), 2);
        assert_eq!(skills[0].name, "alpha-skill");
        assert_eq!(skills[0].description, "Does alpha things");
        assert_eq!(skills[1].name, "zeta-skill");
        assert_eq!(skills[1].description, "Does zeta things");
    }

    /// A subdirectory with no `SKILL.md` is skipped, not an error.
    ///
    /// Why: `.claude/skills/` may contain scratch or in-progress
    /// directories; discovery must degrade gracefully (no panic).
    /// Test: this test.
    #[test]
    fn discover_skill_metadata_skips_dir_without_skill_md() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join("not-a-skill")).expect("mkdir");
        write_skill(
            tmp.path(),
            "real-skill",
            "---\nname: real-skill\ndescription: A real one\n---\n",
            "body\n",
        );

        let skills = discover_skill_metadata(tmp.path());
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "real-skill");
    }

    /// A missing skills directory falls back to the embedded default
    /// catalog, not an empty one (#2895).
    ///
    /// Why: Most projects will not have a `.claude/skills/` dir at all; a
    /// fresh project must still see trusty-mpm's universal skill set rather
    /// than starting with zero skills.
    /// What: Discover from a nonexistent path; expect the full embedded
    /// catalog (28 skills, sorted by name — matches
    /// `crate::assets::tests::default_skills_names_are_unique`'s count).
    /// Test: this test.
    #[test]
    fn discover_skill_metadata_falls_back_to_embedded_when_disk_empty() {
        let skills = discover_skill_metadata(Path::new("/nonexistent/skills/dir"));
        assert_eq!(skills.len(), 28);
        assert!(skills.iter().any(|s| s.name == "systematic-debugging"));
        assert!(
            skills.windows(2).all(|w| w[0].name <= w[1].name),
            "embedded fallback must be sorted by name"
        );
    }

    /// A disk directory with even one discoverable skill is used as-is; the
    /// embedded defaults are never merged in alongside it.
    ///
    /// Why: Pins the fallback threshold (empty disk scan only) so a project
    /// that has deliberately curated its own skill catalog is not silently
    /// joined by the 28 embedded defaults it did not ask for.
    /// Test: this test.
    #[test]
    fn discover_skill_metadata_disk_wins_when_present() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_skill(
            tmp.path(),
            "only-skill",
            "---\nname: only-skill\ndescription: The only one\n---\n",
            "body\n",
        );

        let skills = discover_skill_metadata(tmp.path());
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "only-skill");
    }

    /// A `SKILL.md` with no frontmatter fence falls back to the directory
    /// name and an empty description, rather than being rejected.
    ///
    /// Why: Malformed skill files must degrade gracefully (no panic).
    /// Test: this test.
    #[test]
    fn discover_skill_metadata_falls_back_to_dirname() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_skill(tmp.path(), "no-frontmatter", "", "# Just a body\n");

        let skills = discover_skill_metadata(tmp.path());
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "no-frontmatter");
        assert_eq!(skills[0].description, "");
    }

    /// `load_skill_body` returns the body with frontmatter stripped, for a
    /// name present in the known catalog.
    ///
    /// Why: This is the lazy on-invoke load path.
    /// Test: this test.
    #[test]
    fn load_skill_body_returns_body_without_frontmatter() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_skill(
            tmp.path(),
            "demo-skill",
            "---\nname: demo-skill\ndescription: Demo\n---\n",
            "# Demo Skill\nFull instructions here.\n",
        );
        let known = discover_skill_metadata(tmp.path());

        let body = load_skill_body(tmp.path(), "demo-skill", &known).expect("load body");
        assert!(body.contains("# Demo Skill"));
        assert!(body.contains("Full instructions here."));
        assert!(!body.contains("description: Demo"));
    }

    /// `load_skill_body` rejects a name absent from the known catalog.
    ///
    /// Why: This is the safety guard against an LLM hallucinating a skill
    /// name — no path is ever built from a name outside the discovered set.
    /// What: `tmp` is an empty tempdir, so `discover_skill_metadata` returns
    /// the embedded-fallback catalog (the 28 names in
    /// `crate::assets::DEFAULT_SKILLS`, #2895), not a truly empty one;
    /// `"ghost-skill"` is absent from that catalog too, so this now exercises
    /// unknown-name rejection against the embedded fallback rather than an
    /// empty set — the guard behaves identically either way.
    /// Test: this test.
    #[test]
    fn load_skill_body_rejects_unknown_name() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let known = discover_skill_metadata(tmp.path());
        let err = load_skill_body(tmp.path(), "ghost-skill", &known);
        assert!(matches!(err, Err(SkillError::Unknown(_))));
    }

    /// `load_skill_body` falls back to `crate::assets::DEFAULT_SKILLS` when
    /// the disk read fails but the name is a known embedded default (#2895).
    ///
    /// Why: `known` may itself have come from `embedded_skill_metadata` (an
    /// empty disk dir); the body loader must be able to resolve those names
    /// too, not just names discovered from real files.
    /// What: An empty tempdir (so `discover_skill_metadata` returns the
    /// embedded catalog), then `load_skill_body` for a real embedded skill
    /// name returns its body without hitting disk.
    /// Test: this test.
    #[test]
    fn load_skill_body_falls_back_to_embedded_when_disk_read_fails() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let known = discover_skill_metadata(tmp.path());
        let body =
            load_skill_body(tmp.path(), "systematic-debugging", &known).expect("embedded body");
        assert!(!body.trim().is_empty());
        assert!(
            !body.contains("name: systematic-debugging"),
            "frontmatter must be stripped"
        );
    }

    /// `format_skill_catalog` returns an empty string for an empty catalog.
    ///
    /// Why: An empty section must contribute nothing to the assembled
    /// prompt, matching `assemble_system_prompt`'s empty-section-skipping.
    /// Test: this test.
    #[test]
    fn format_skill_catalog_empty_is_empty_string() {
        assert_eq!(format_skill_catalog(&[]), "");
    }

    /// `format_skill_catalog` lists every skill as a `- name: description`
    /// line.
    ///
    /// Why: This is the token-efficient catalog the prompt actually sees.
    /// Test: this test.
    #[test]
    fn format_skill_catalog_lists_every_skill() {
        let skills = vec![
            SkillMetadata {
                name: "alpha".into(),
                description: "First".into(),
            },
            SkillMetadata {
                name: "beta".into(),
                description: "Second".into(),
            },
        ];
        let catalog = format_skill_catalog(&skills);
        assert!(catalog.contains("- alpha: First"));
        assert!(catalog.contains("- beta: Second"));
    }

    /// `FsSkillResolver::resolve` lazily loads a known skill's body.
    ///
    /// Why: Verifies the concrete `SkillResolver` impl end-to-end.
    /// Test: this test.
    #[test]
    fn fs_skill_resolver_resolves_known_skill() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_skill(
            tmp.path(),
            "demo-skill",
            "---\nname: demo-skill\ndescription: Demo\n---\n",
            "Full body content.\n",
        );
        let resolver = FsSkillResolver::new(tmp.path());
        let body = resolver.resolve("demo-skill").expect("resolve");
        assert!(body.contains("Full body content."));
    }

    /// `FsSkillResolver::list` returns exactly the discovered metadata names.
    ///
    /// Test: this test.
    #[test]
    fn fs_skill_resolver_list_matches_metadata_names() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_skill(
            tmp.path(),
            "demo-skill",
            "---\nname: demo-skill\ndescription: Demo\n---\n",
            "body\n",
        );
        let resolver = FsSkillResolver::new(tmp.path());
        assert_eq!(resolver.list(), vec!["demo-skill".to_string()]);
        assert_eq!(resolver.metadata().len(), 1);
    }

    /// `FsSkillResolver::resolve` returns `None` for an unknown skill name.
    ///
    /// Why: Guards against a panic or leaked IO error surfacing through the
    /// trait's `Option`-returning API.
    /// What: `tmp` is an empty tempdir, so the resolver's cached metadata is
    /// the embedded-fallback catalog (the 28 names in
    /// `crate::assets::DEFAULT_SKILLS`, #2895), not a truly empty one;
    /// `"ghost"` is absent from that catalog too, so this now exercises
    /// unknown-name resolution against the embedded fallback rather than an
    /// empty set.
    /// Test: this test.
    #[test]
    fn fs_skill_resolver_resolve_unknown_returns_none() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let resolver = FsSkillResolver::new(tmp.path());
        assert!(resolver.resolve("ghost").is_none());
    }
}
