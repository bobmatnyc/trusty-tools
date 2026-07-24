//! Per-call root resolution + path confinement.
//!
//! Why: one MCP service instance serves EVERY assistant's KB tree — there is no
//! single pinned root. Each tool call resolves its own root, and every file
//! operation must be confined under a knowledge-base directory so a
//! collection/name argument can never traverse out of the tree (`..`, absolute
//! paths, or a symlink escaping the root).
//!
//! What: [`Roots`] holds the service defaults (knowledge_dir + default root).
//! [`Roots::resolve`] applies the precedence `explicit root arg → agent arg
//! (mapped to <knowledge_dir>/<agent-slug>) → service default`. [`confine`]
//! joins caller-supplied relative components under a resolved root, rejecting
//! any traversal, and [`assert_within`] verifies a canonicalised path stays
//! under its root (symlinks not followed out).
//!
//! Test: `resolve_precedence`, `agent_maps_under_knowledge_dir`,
//! `confine_rejects_traversal`, `assert_within_blocks_escape`.

use std::path::{Component, Path, PathBuf};

use crate::entity::slugify;

/// Service-level root configuration, shared across every tool call.
#[derive(Debug, Clone)]
pub struct Roots {
    /// Directory holding one subtree per assistant (default
    /// `~/.trusty-agents/knowledge`). All agent-mapped and enumerated trees live
    /// here.
    pub knowledge_dir: PathBuf,
    /// The service default root when neither `root` nor `agent` is supplied
    /// (default `<knowledge_dir>/bob-kb`).
    pub default_root: PathBuf,
}

impl Roots {
    /// Build a [`Roots`] from a knowledge dir and default root.
    pub fn new(knowledge_dir: PathBuf, default_root: PathBuf) -> Self {
        Self {
            knowledge_dir,
            default_root,
        }
    }

    /// Resolve the KB root for a single tool call.
    ///
    /// Why: every tool accepts optional `root` and `agent` arguments; the
    /// resolution order is fixed so one service can address any tree.
    /// What: precedence — an explicit `root` (used verbatim) → an `agent`
    /// (mapped to `<knowledge_dir>/<slug(agent)>`) → the service `default_root`.
    /// Test: `resolve_precedence`, `agent_maps_under_knowledge_dir`.
    pub fn resolve(&self, root: Option<&str>, agent: Option<&str>) -> PathBuf {
        if let Some(r) = root.filter(|s| !s.is_empty()) {
            return PathBuf::from(r);
        }
        if let Some(a) = agent.filter(|s| !s.is_empty()) {
            return self.knowledge_dir.join(slugify(a));
        }
        self.default_root.clone()
    }

    /// The path of the tree directory for a given agent slug (for enumeration).
    pub fn tree_path(&self, agent_slug: &str) -> PathBuf {
        self.knowledge_dir.join(agent_slug)
    }
}

/// Join caller-supplied relative segments under `root`, rejecting traversal.
///
/// Why: a `collection`/`name` argument is attacker-controlled input; it must
/// never contain `..`, a root/prefix component, or an absolute path.
/// What: validates every segment is a plain normal path component (no
/// `ParentDir`, `RootDir`, or `Prefix`) and joins them onto `root`. Returns an
/// error describing the rejected segment otherwise.
/// Test: `confine_rejects_traversal`.
pub fn confine(root: &Path, segments: &[&str]) -> anyhow::Result<PathBuf> {
    let mut out = root.to_path_buf();
    for seg in segments {
        if seg.is_empty() {
            anyhow::bail!("empty path segment is not allowed");
        }
        let rel = Path::new(seg);
        for comp in rel.components() {
            match comp {
                Component::Normal(part) => out.push(part),
                other => anyhow::bail!("path segment {seg:?} contains illegal component {other:?}"),
            }
        }
    }
    Ok(out)
}

/// Assert a (possibly not-yet-existing) path stays under `root` once resolved.
///
/// Why: confinement checks the literal segments, but a symlink inside the tree
/// could still redirect out; the final safety net is a canonicalised prefix
/// check.
/// What: canonicalises `root` and the nearest existing ancestor of `path`, and
/// verifies the resolved ancestor is `root` or below it. A path whose ancestor
/// resolves outside `root` (e.g. via a symlink) is rejected. When `root` itself
/// does not yet exist, the check is skipped (nothing to escape from).
/// Test: `assert_within_blocks_escape`.
pub fn assert_within(root: &Path, path: &Path) -> anyhow::Result<()> {
    let canon_root = match root.canonicalize() {
        Ok(r) => r,
        Err(_) => return Ok(()), // root not created yet — no boundary to breach
    };
    // Canonicalise the nearest existing ancestor of `path` (it may not exist).
    let mut probe = path.to_path_buf();
    let canon_probe = loop {
        match probe.canonicalize() {
            Ok(p) => break p,
            Err(_) => match probe.parent() {
                Some(parent) if parent != probe => probe = parent.to_path_buf(),
                _ => return Ok(()), // nothing resolvable to compare — literal check already passed
            },
        }
    };
    if canon_probe.starts_with(&canon_root) {
        Ok(())
    } else {
        anyhow::bail!(
            "resolved path {} escapes root {}",
            canon_probe.display(),
            canon_root.display()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roots() -> Roots {
        Roots::new(
            PathBuf::from("/kb/knowledge"),
            PathBuf::from("/kb/knowledge/bob-kb"),
        )
    }

    /// Why: the resolution precedence is the multi-tree contract.
    /// What: asserts explicit root wins, then agent, then default.
    /// Test: self-contained.
    #[test]
    fn resolve_precedence() {
        let r = roots();
        assert_eq!(
            r.resolve(Some("/custom"), Some("izzie")),
            PathBuf::from("/custom")
        );
        assert_eq!(r.resolve(None, None), PathBuf::from("/kb/knowledge/bob-kb"));
    }

    /// Why: an agent name must map deterministically under knowledge_dir and be
    /// slugged (so "CTO Bot" cannot inject path separators).
    /// What: asserts the agent maps to `<knowledge_dir>/<slug>`.
    /// Test: self-contained.
    #[test]
    fn agent_maps_under_knowledge_dir() {
        let r = roots();
        assert_eq!(
            r.resolve(None, Some("CTO Bot")),
            PathBuf::from("/kb/knowledge/cto-bot")
        );
    }

    /// Why: traversal via a caller-supplied segment is the primary attack.
    /// What: asserts `..`, absolute, and root-prefixed segments are rejected,
    /// and a normal segment is accepted.
    /// Test: self-contained.
    #[test]
    fn confine_rejects_traversal() {
        let root = Path::new("/kb/knowledge/bob-kb");
        assert!(confine(root, &["people", "ada.md"]).is_ok());
        assert!(confine(root, &["..", "escape"]).is_err());
        assert!(confine(root, &["people/../../etc"]).is_err());
        assert!(confine(root, &["/etc/passwd"]).is_err());
    }

    /// Why: a symlink inside the tree could still redirect writes outside it;
    /// the canonicalised prefix check is the backstop.
    /// What: builds a real temp root, a sibling outside it, and a symlink from
    /// inside the root to the sibling; asserts a path through the symlink is
    /// rejected while an in-root path is accepted.
    /// Test: self-contained.
    #[test]
    fn assert_within_blocks_escape() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        // In-root path is fine.
        assert!(assert_within(&root, &root.join("people/ada.md")).is_ok());
        // Symlink inside root pointing outside → escape.
        let link = root.join("evil");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside, &link).unwrap();
            assert!(assert_within(&root, &link.join("passwd")).is_err());
        }
        #[cfg(not(unix))]
        let _ = link;
    }
}
