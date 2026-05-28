//! Filesystem-backed `SkillResolver`.
//!
//! Why: Agents need to pull in domain-specific Markdown guidance on demand.
//! A resolver abstraction lets tests use an in-memory map while production
//! walks a known directory hierarchy.
//! What: `FsSkillResolver` implements `SkillResolver` with the search order:
//!   1. `{project_root}/.claude/skills/{name}/SKILL.md`
//!   2. `{home}/.claude/skills/{name}/SKILL.md`
//!   3. `{project_root}/.open-mpm/skills/{name}.md`
//! Test: `super::fs_resolver_*` cases in the parent module's test block.

use std::path::PathBuf;

use crate::tools::traits::SkillResolver;

/// Filesystem-backed skill resolver.
pub struct FsSkillResolver {
    project_root: PathBuf,
    home: Option<PathBuf>,
}

impl FsSkillResolver {
    /// Build a resolver from a project root and optional home dir.
    pub fn new(project_root: PathBuf, home: Option<PathBuf>) -> Self {
        Self { project_root, home }
    }

    /// Build with sensible defaults: CWD as project root, `$HOME` as home.
    pub fn from_defaults() -> Self {
        let project_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let home = std::env::var_os("HOME").map(PathBuf::from);
        Self::new(project_root, home)
    }

    fn candidate_paths(&self, name: &str) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        paths.push(
            self.project_root
                .join(".claude")
                .join("skills")
                .join(name)
                .join("SKILL.md"),
        );
        if let Some(home) = &self.home {
            paths.push(
                home.join(".claude")
                    .join("skills")
                    .join(name)
                    .join("SKILL.md"),
            );
        }
        paths.push(
            self.project_root
                .join(".open-mpm")
                .join("skills")
                .join(format!("{name}.md")),
        );
        paths
    }
}

impl SkillResolver for FsSkillResolver {
    fn resolve(&self, name: &str) -> Option<String> {
        for p in self.candidate_paths(name) {
            if p.exists()
                && let Ok(s) = std::fs::read_to_string(&p)
            {
                return Some(s);
            }
        }
        None
    }

    fn list(&self) -> Vec<String> {
        let mut out = Vec::new();
        // Search .claude/skills/* (directories with SKILL.md)
        let bases = {
            let mut b: Vec<PathBuf> = vec![self.project_root.join(".claude").join("skills")];
            if let Some(home) = &self.home {
                b.push(home.join(".claude").join("skills"));
            }
            b
        };
        for base in bases {
            if let Ok(entries) = std::fs::read_dir(&base) {
                for e in entries.flatten() {
                    if e.path().join("SKILL.md").exists()
                        && let Some(name) = e.file_name().to_str()
                    {
                        out.push(name.to_string());
                    }
                }
            }
        }
        // .open-mpm/skills/*.md
        if let Ok(entries) = std::fs::read_dir(self.project_root.join(".open-mpm").join("skills")) {
            for e in entries.flatten() {
                if let Some(ext) = e.path().extension()
                    && ext == "md"
                    && let Some(stem) = e.path().file_stem().and_then(|s| s.to_str())
                {
                    out.push(stem.to_string());
                }
            }
        }
        out.sort();
        out.dedup();
        out
    }
}
