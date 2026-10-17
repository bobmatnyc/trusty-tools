//! The bundled agent and workflow trees, read at runtime from instructional
//! content (ADR-0064, #9014).
//!
//! Why: ADR-0064 makes agents runtime-only content. A published crate cannot
//! embed a file outside its own directory, and a content fix must not need a
//! crate release. So the roster and the workflow definitions `tagent` deploys
//! to `~/.trusty-agents/` live under `content/agents/trusty-agents/` and are
//! read through the resolver every harness shares: a trusty-tools checkout
//! above the cwd, else the sha256-pinned bundle `tm content update` installed.
//! What: [`BundledTree::from_content`] reads one tree from a resolved source;
//! [`BundledTree::load`] does it for this process. An absent tree, or one
//! missing a [`BundledTreeKind::required`] file, is an [`AgentContentError`]
//! that names the bundle path; nothing here answers with a partial or empty
//! set.
//! Test: `a_tree_missing_a_required_file_fails_closed_naming_it`,
//! `an_absent_tree_fails_closed_naming_its_first_required_file`.
//!
//! # Spec References
//! - ADR-0064 decision 5: `docs/adr/0064-instructional-content-tracked-separately-from-code.md`

use trusty_agents_common::agent_content::{
    AGENTS_CLASS, AgentContentError, ResolvedContent, describe_source,
};

/// Bundle directory of trusty-agents' own trees, nested under `agents/` so the
/// shared roster (`AgentRoster`, top-level `agents/*.md` only) never lists them.
pub const BUNDLE_DIR: &str = "agents/trusty-agents";

/// Which bundled tree: the agent roster or the workflow definitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BundledTreeKind {
    /// Deployed to `~/.trusty-agents/agents/`.
    Agents,
    /// Deployed to `~/.trusty-agents/workflows/`.
    Workflows,
}

impl BundledTreeKind {
    /// The tree's directory name, both under [`BUNDLE_DIR`] and under
    /// `~/.trusty-agents/`.
    pub fn dir_name(self) -> &'static str {
        match self {
            Self::Agents => "agents",
            Self::Workflows => "workflows",
        }
    }

    /// The tree's bundle path, `agents/trusty-agents/<dir_name>`.
    pub fn bundle_path(self) -> String {
        format!("{BUNDLE_DIR}/{}", self.dir_name())
    }

    /// Files the tree must hold, relative to [`Self::bundle_path`].
    ///
    /// Why: the default personas (`ctrl`, `assistant`), the `pm` self-project
    /// marker and the `prescriptive` workflow are what startup and
    /// `--workflow` reach for; a content release without one must fail the
    /// load naming it, not deploy a roster that silently lacks it.
    pub fn required(self) -> &'static [&'static str] {
        match self {
            Self::Agents => &[
                "ctrl.toml",
                "pm.toml",
                "assistant/agent.toml",
                "assistant/persona.md",
            ],
            Self::Workflows => &["prescriptive.json"],
        }
    }
}

/// Every file one bundled tree deploys, as `(path relative to the tree, bytes)`.
///
/// Why: replaces the two `rust-embed` trees (#9014) with the same
/// `(path, bytes)` pairs, so the stamp, provenance and write loop are
/// unchanged.
/// What: built only by [`BundledTree::from_content`], which never returns an
/// empty tree or one missing a required file.
/// Test: `the_repository_trees_load`.
#[derive(Debug, Clone)]
pub struct BundledTree {
    kind: BundledTreeKind,
    files: Vec<(String, Vec<u8>)>,
}

impl BundledTree {
    /// Reads the `kind` tree from `content`.
    ///
    /// What: lists `agents/`, keeps the paths under [`BundledTreeKind::bundle_path`]
    /// that pass the bundled-asset allowlist (a stray file is dropped with a
    /// `warn`), and reads each.
    ///
    /// # Errors
    /// [`AgentContentError::Missing`] naming the first absent required file,
    /// [`AgentContentError::Invalid`] for an empty one, and the resolver's own
    /// error when a listed file cannot be read.
    /// Test: `a_tree_missing_a_required_file_fails_closed_naming_it`,
    /// `an_empty_required_file_is_invalid`.
    pub fn from_content(
        content: &ResolvedContent,
        kind: BundledTreeKind,
    ) -> Result<Self, AgentContentError> {
        let prefix = format!("{}/", kind.bundle_path());
        let mut files = Vec::new();
        // #9014: `list` of a nested destination is empty in dev mode, so list
        // the class and filter by prefix; both backings agree on that.
        for key in content.list(AGENTS_CLASS)? {
            let Some(rel) = key.strip_prefix(&prefix) else {
                continue;
            };
            if !super::is_bundled_asset(rel) {
                tracing::warn!(
                    asset = %key,
                    "ignoring a file in the bundled tree that is not a bundled asset"
                );
                continue;
            }
            files.push((rel.to_owned(), content.read(&key)?));
        }
        let origin = || describe_source(content.source());
        for required in kind.required() {
            let path = format!("{prefix}{required}");
            match files.iter().find(|(rel, _)| rel == required) {
                None => {
                    return Err(AgentContentError::Missing {
                        origin: origin(),
                        path,
                    });
                }
                Some((_, bytes)) if bytes.iter().all(u8::is_ascii_whitespace) => {
                    return Err(AgentContentError::Invalid {
                        origin: origin(),
                        path,
                        reason: "the file is empty".to_owned(),
                    });
                }
                Some(_) => {}
            }
        }
        Ok(Self { kind, files })
    }

    /// [`BundledTree::from_content`] over the content resolved for this
    /// process (`trusty_code::assets::content::resolve_for_process`).
    pub fn load(kind: BundledTreeKind) -> Result<Self, AgentContentError> {
        Self::from_content(&trusty_code::assets::content::resolve_for_process()?, kind)
    }

    /// Which tree this is.
    pub fn kind(&self) -> BundledTreeKind {
        self.kind
    }

    /// Every `(relative path, bytes)` pair; never empty.
    pub fn files(&self) -> &[(String, Vec<u8>)] {
        &self.files
    }

    /// The bytes of `rel` (`assistant/agent.toml`), if the tree holds it.
    pub fn get(&self, rel: &str) -> Option<&[u8]> {
        self.files
            .iter()
            .find(|(r, _)| r == rel)
            .map(|(_, bytes)| bytes.as_slice())
    }

    /// Test-only: a tree over arbitrary files, bypassing the required-file
    /// check, so the deploy loop's own filters can be exercised.
    #[cfg(test)]
    pub(super) fn from_files(kind: BundledTreeKind, files: Vec<(String, Vec<u8>)>) -> Self {
        Self { kind, files }
    }
}

/// Test helpers for every module that reads the bundled trees (#9014).
///
/// Why: the trees left the crate directory, so a test that read
/// `CARGO_MANIFEST_DIR/.trusty-agents/agents` now reads the checkout's
/// `content/`. A fake checkout keeps the resolver in the path without
/// needing the real checkout's `.git` (EVO syncs none, #9619).
#[cfg(test)]
pub(crate) mod test_support {
    use std::path::{Path, PathBuf};
    use std::sync::OnceLock;

    use super::{BundledTree, BundledTreeKind};

    /// `content/<kind bundle path>` in the checkout under test, on disk.
    ///
    /// What: the runtime checkout (#9298), never the compile-time build path.
    pub(crate) fn repo_tree_dir(kind: BundledTreeKind) -> PathBuf {
        trusty_common::test_harness::test_repo_root()
            .expect("resolve the trusty-tools checkout (set TRUSTY_TEST_REPO_ROOT)")
            .join("content")
            .join(kind.bundle_path())
    }

    /// Builds a trusted checkout at `root` holding `files` (repo-relative
    /// path, bytes), with every dev class directory present.
    pub(crate) fn fake_checkout(root: &Path, files: &[(String, Vec<u8>)]) {
        for (_, rel) in trusty_common::content::DEV_CLASS_SOURCES {
            std::fs::create_dir_all(root.join(rel)).expect("class dir");
        }
        std::fs::create_dir_all(root.join(".git")).expect(".git");
        std::fs::write(root.join("Cargo.toml"), "[workspace]\n").expect("Cargo.toml");
        for (rel, body) in files {
            let path = root.join(rel);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
            std::fs::write(path, body).expect("file");
        }
    }

    /// Every regular file under [`repo_tree_dir`] as `(repo-relative path,
    /// bytes)`, the way `fake_checkout` takes them.
    pub(crate) fn repo_tree_files(kind: BundledTreeKind) -> Vec<(String, Vec<u8>)> {
        fn walk(dir: &Path, key: &str, out: &mut Vec<(String, Vec<u8>)>) {
            for entry in std::fs::read_dir(dir).expect("bundled tree dir").flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with('.') {
                    continue;
                }
                let child = format!("{key}/{name}");
                if entry.path().is_dir() {
                    walk(&entry.path(), &child, out);
                } else {
                    out.push((child, std::fs::read(entry.path()).expect("bundled file")));
                }
            }
        }
        let mut out = Vec::new();
        walk(
            &repo_tree_dir(kind),
            &format!("content/{}", kind.bundle_path()),
            &mut out,
        );
        out
    }

    /// The checkout's `kind` tree, read through the resolver from a fake
    /// checkout copy; loaded once per test process.
    pub(crate) fn repo_tree(kind: BundledTreeKind) -> &'static BundledTree {
        static AGENTS: OnceLock<BundledTree> = OnceLock::new();
        static WORKFLOWS: OnceLock<BundledTree> = OnceLock::new();
        let cell = match kind {
            BundledTreeKind::Agents => &AGENTS,
            BundledTreeKind::Workflows => &WORKFLOWS,
        };
        cell.get_or_init(|| {
            let tmp = tempfile::tempdir().expect("tempdir");
            fake_checkout(tmp.path(), &repo_tree_files(kind));
            let content = trusty_agents_common::agent_content::checkout_content(tmp.path())
                .expect("fake checkout resolves");
            BundledTree::from_content(&content, kind).expect("the checkout's bundled tree loads")
        })
    }
}

#[cfg(test)]
#[path = "content_tests.rs"]
mod tests;
